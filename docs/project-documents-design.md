# MemStack 项目文档工作区优化设计文档 (V1)

## 1. 概述 (Overview)

### 1.1. 背景与目标

项目文档功能当前的草稿审核流程（生成 → 编辑 → 逐份批准 → 自动晋升）已经完整，
但文档晋升为正式文档之后，桌面端体验出现明显断层：

- **正式文档完全只读**：改一个错别字都需要开 AI 会话走 MCP 工具。
- **看不到 AI 改了什么**：版本号 +1 但无 diff 视图；恢复上一版的能力也没有界面入口。
- **AI 更新无感知**：只能手动点刷新才知道文档被更新。
- **长文档浏览吃力**：CHANGELOG / CURRENT_STATUS 持续膨胀，无大纲、无搜索，归档规则已定但未实施。

**核心目标：**

1. **补齐写入闭环**：桌面端可直接编辑正式文档，并提供版本对比与一键恢复上一版。
2. **更新自动感知**：AI（或外部编辑器）更新文档后，界面自动感知并提示。
3. **长文档治理**：落实既定归档规则，配合大纲导航与文档内搜索，让长文档可读、可查。

### 1.2. 术语定义

- **正式文档 (Formal Document)**：五份晋升后的项目文档（CONTEXT / DECISIONS / CURRENT_STATUS / PROBLEMS / CHANGELOG），落盘于工作空间 `.memstack/` 目录，数据库持有只读镜像。
- **上一版快照 (Previous Snapshot)**：`project_document` 表中的 `previous_content` / `previous_checksum` / `previous_version` 三列，每次更新前保留一份旧内容，仅一级深度。
- **乐观锁 (Optimistic Locking)**：更新请求必须携带 `expectedVersion`，与服务端当前版本不符则整批拒绝。
- **归档 (Archive)**：活动文档超过阈值后，将最旧条目搬运到 `.memstack/archive/` 下按日期存放的文件，活动文档只保留最近条目。
- **Watcher 领导权 (Watch Leadership)**：跨进程 NamedMutex 选举出的唯一文件监听者；同一时刻只有一个进程读取文件变更并触发同步回调。

---

## 2. 现状盘点 (Current State)

### 2.1. 能力矩阵

| 环节 | 现状 | 来源 |
| --- | --- | --- |
| 草稿审核 | 完整：预览/原文切换、编辑、批准/撤销、第五份批准自动晋升 | `ProjectDocumentsPage.vue` |
| 正式文档读取 | 只读渲染 + 原文查看，格式错误可自动修复 | 同上 |
| 正式文档写入 | 仅 MCP 侧 `batch_update(workspace_path, …)`，桌面端无入口 | `project_document_service.rs` |
| 版本恢复 | 仅 MCP 侧 `restore_previous_versions(workspace_path, …)`，界面无入口 | 同上 |
| 文件监听 | `project_document_watcher` 跨进程领导权选举；MCP 进程在 `handoff` 时注册并持有 | 同上 |
| 前端事件通道 | 无（前端经 `api.ts` 路由层 invoke Tauri 命令，无 listen/emit 使用） | `api.ts` / `main.rs` |
| 归档 | 规则已定（见 §6.1），代码未实施 | 方向2 设计 |

### 2.2. 关键技术事实

1. `batch_update` / `restore_previous_versions` 只接受 `workspace_path`（MCP 调用方），
   桌面端持有的是 `projectId`；service 层已有 `get_project_settings(project_id)` 可解析绑定关系。
2. watcher 领导权由 MCP 进程在 `handoff` 时注册持有；桌面端再注册也收不到回调
   （跨进程只有一个 leader 读文件），桌面侧"事件推送"不能直接复用 watcher。
3. 前端虽无事件通道，但 `@tauri-apps/api` v2 已在依赖中，`listen` 可直接使用；后端托盘已持有 `app_handle`。
4. schema 中每份文档仅保留**一级**上一版快照，无完整历史版本表。
5. `batch_update_locked` 内部已有整批校验（版本 + 格式）、工作空间同步锁（`with_workspace_sync_lock`）
   与条目解析器（`first_h2_entries` / `extract_section` / `collect_problem_ids`），归档可直接复用。

---

## 3. 需求分析 (Requirements)

### 3.1. 功能性需求

**方向 A —— 正式文档编辑 + diff/回滚**

1. 桌面端可对 ACTIVE 状态的单份正式文档发起编辑，保存走乐观锁整批校验。
2. 提供当前版本与上一版快照的行级 diff 对比视图。
3. diff 视图内提供「恢复上一版」操作（确认弹窗），恢复同样走乐观锁。

**方向 B —— 更新自动感知**

1. 页面可见时，文档被 AI / 外部编辑器更新后，界面自动刷新概览与详情，并给出轻提示。
2. 页面不可见时不做无效轮询。

**方向 C —— 归档 + 大纲导航**

1. CHANGELOG 条目 > 20 条时，最旧溢出条目按标题日期写入 `archive/changelog/<日期>.md`，活动文档保留最近 20 条。
2. PROBLEMS「已解决问题」> 10 条时按解决日期归档到 `archive/problems/<日期>.md`。
3. 归档文件仅在存在变更的日期生成；不参与版本管理与镜像同步。
4. 读取侧提供归档日期列表与单日内容查看（日期选择器切换）。
5. 文档内容区提供大纲导航（h2/h3）与文档内搜索（命中高亮 + 上下跳转）。

### 3.2. 非功能性需求

- **并发安全**：桌面编辑与 AI 会话并发写入时，乐观锁整批拒绝，行为与 MCP 侧一致。
- **事务守恒**：归档失败不得阻断主更新流程；归档搬运可回滚、不丢条目。
- **零新增重依赖**：diff 采用纯 TS 行级 LCS 实现；大纲/搜索为纯前端能力。
- **体验一致**：编辑交互与现有草稿编辑保持一致（textarea + 预览/原文切换）。

---

## 4. 系统设计 (System Design)

### 4.1. 总体架构

三个方向按依赖关系排序实施：

```
A 编辑+diff/回滚 ──► B1 轮询感知 ──► C1 归档 ──► C2 大纲/搜索
       │
       └── A 先行的原因：归档触发点挂在 batch_update_locked 提交后，
           桌面编辑与 MCP 写入走同一条 service 路径，归档只有一条触发链。
```

### 4.2. 方向 A：正式文档编辑 + diff/回滚

#### A1. 后端：project_id 变体命令

在 `project_document_service.rs` 新增三个方法（内部复用现有 locked 逻辑，不复制实现）：

| 方法 | 说明 |
| --- | --- |
| `batch_update_by_project(project_id, updates)` | 先 `get_project_settings` 解析 workspace_path，再走 `with_workspace_sync_lock` + `batch_update_locked` |
| `restore_previous_by_project(project_id, documents)` | 同上，复用 `restore_previous_versions_locked` |
| `get_document_previous(project_id, document_type)` | 返回上一版快照（content / version / checksum），供 diff 展示；无快照时返回 None |

配套新增 Tauri 命令（`src-tauri/src/commands/project_documents.rs`）与 `api.ts` 路由，模式与现有命令完全一致。

**安全模型**：桌面端是本机用户 UI（固定 caller，ReadWrite，无项目绑定），与 `repair_document` /
`promote_drafts` 同等信任级别，不引入新权限面。

**并发保护**：`expectedVersion` 整批校验。AI 会话同时写入 → 版本冲突
（`PROJECT_DOCUMENT_VERSION_CONFLICT`）→ 前端提示刷新后重试。

#### A2. 前端：编辑与对比体验

- 正式文档头部加「编辑」按钮 → textarea 编辑模式（与草稿编辑交互一致），
  保存携带 `expectedVersion` + changeSummary（默认「用户在 MemStack 中编辑」）。
- 「版本对比」按钮 → diff 视图：新增纯 TS 行级 diff 工具（LCS 算法，零依赖，配 vitest 单测），
  左右分栏或统一视图渲染增/删/改行。
- diff 视图内「恢复上一版」按钮，复用现有 `CenteredConfirmDialog.vue` 二次确认。

**限制说明**：v1 只做一级对比/回滚（schema 现状）。恢复后当前版本会变成新的「上一版」，
误恢复也能再撤销一次。多级历史需要新表（如 `project_document_version`），按需再议。

### 4.3. 方向 B：AI 更新自动感知

**B1（推荐先做）—— 前端轻量轮询：**

- 页面可见时（`document.visibilityState`）每 10s 拉取 overview（本地 SQLite 开销可忽略）。
- 对比 documents 的 `version` / `updated_at`，无变化不刷新；有变化才刷新详情 + toast「项目文档已更新」。
- 覆盖两类更新源：MCP `batch_update`（直接写 DB）；外部编辑器改文件（MCP leader watcher 同步落 DB）。
- 零后端改动。

**B2（可选增强，暂缓）—— watcher 租约 + Tauri event 推送：**

- 桌面进程对当前查看的项目注册 watcher 租约，回调中 `app_handle.emit("project-document-changed", …)`，
  前端 `listen` 消费。
- 受领导权制约：MCP 进程存活时桌面拿不到回调，需退化为轮询，复杂度高、收益低。
- 结论：观察 B1 效果后再决定是否实施。

### 4.4. 方向 C：归档 + 大纲导航

#### C1. 归档执行器（4 步实施）

1. **归档执行器**：挂在 `batch_update_locked` 提交后的后置步骤；
   - 计数与切分复用现有解析器（`first_h2_entries` / `extract_section` / `collect_problem_ids`）。
   - 失败仅记日志、不阻断主更新；下次更新时补偿重试（阈值判断天然幂等）。
   - 结构异常（条目无法解析出日期等）时跳过归档、不阻断更新。
2. **归档读写 + journal 恢复机制接入**：搬运走现有恢复机制，失败可回滚、不丢条目。
3. **读取侧 API**：`list_archives(project_id, kind)` 返回日期列表；
   `get_archive(project_id, kind, date)` 返回单日内容；新增 Tauri 命令 + 路由。
4. **前端**：文档列表底部「历史归档」入口 + 日期选择器（按年月分组），归档内容只读渲染。

#### C2. 大纲 + 搜索（纯前端）

- `markdown.ts` 渲染时为 h2/h3 生成稳定 id；新增 `extractToc(content)` 工具（配单测）。
- 文档内容区：宽屏右侧 sticky 目录；窄屏折叠为下拉；点击滚动定位。
- 文档内搜索：内容区顶部搜索框，命中高亮 `<mark>` + 上/下跳转（渲染前对原文做标记与转义处理）。

---

## 5. 实施计划 (Implementation Plan)

| 阶段 | 内容 | 主要改动面 |
| --- | --- | --- |
| 1 | A1 后端 project_id 变体命令 + 单测 | `project_document_service.rs` / `commands/project_documents.rs` / `api.ts` |
| 2 | A2 前端编辑 + diff 工具 + 恢复入口 | `ProjectDocumentsPage.vue` / 新增 `diff.ts` + 单测 |
| 3 | B1 轮询感知 | `ProjectDocumentsPage.vue` |
| 4 | C1 归档执行器 + 读取侧 API + 前端入口 | service / commands / `api.ts` / 页面 |
| 5 | C2 大纲导航 + 文档内搜索 | `markdown.ts` + 单测 / 页面 |

每阶段独立可验证：Rust 侧 `cargo test` + `clippy` + `rustfmt`；前端 `vitest` + `vue-tsc`。

---

## 6. 既定规则与约束 (Established Rules)

### 6.1. 归档规则（方向2 既定，本设计直接沿用）

- CHANGELOG 条目数超过 20 条时，溢出的最旧条目按其标题日期写入 `archive/changelog/<日期>.md`，
  活动文档只保留最近 20 条。
- PROBLEMS「已解决问题」超过 10 条时按解决日期归档到 `archive/problems/`。
- 归档文件仅在存在变更的日期生成，当天无变更则不产生文件。
- 归档搬运与文件替换走现有 journal 恢复机制，失败可回滚、不丢条目。
- 归档文件不参与版本管理与镜像同步，不增加更新事务负担。
- 读取侧通过日期选择器切换查看。

### 6.2. 风险与对策

| 风险 | 对策 |
| --- | --- |
| 桌面编辑与 AI 并发写入 | 乐观锁整批拒绝，沿用现有冲突错误码与前端提示 |
| 自写文件触发 watcher | 写入在 sync 锁内且 DB 校验和一致，watcher 同步为 no-op（MCP 写入同路径已验证） |
| 归档解析依赖文档结构约定 | 解析器已有单测基础；归档执行器补「结构异常跳过不阻断」防御 + 单测 |
| 只有一级快照，误恢复 | 恢复后旧当前版成为新快照，可再撤销一次；多级历史按需扩展 |

---

## 7. 后续演进 (Future Work)

- **多级版本历史**：新增 `project_document_version` 表，保留最近 N 版，支持任意版本对比与恢复。
- **B2 事件推送**：watcher 领导权广播机制（leader 通知其他进程）或桌面侧独立监听方案。
- **跨文档全文搜索**：五份文档 + 结论卡片统一检索，命中跳转到文档 + 锚点。
