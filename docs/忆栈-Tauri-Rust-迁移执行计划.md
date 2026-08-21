# 忆栈 Tauri + Rust 迁移执行计划

> 文档版本：1.0  
> 制定日期：2026-08-14  
> 当前产品版本：0.4.0  
> 适用项目：`E:\AICoding\mcp-ai-memory\desktop-client`  
> 目标平台：Windows x64  

品牌与进程命名：

```text
产品中文名：忆栈
产品英文标识：MemStack
桌面程序：忆栈.exe
MCP stdio 程序：MemStack-MCP.exe
```

现有 `%LOCALAPPDATA%\UnifiedAiMemory` 数据目录、`UnifiedAiMemory` 命名互斥锁和 DPAPI entropy 属于升级兼容标识，迁移期间继续保留，不随产品显示名称修改。

## 执行进度快照（2026-08-15 更新）

| 阶段 | 状态 | 验收记录 |
|---|---|---|
| 阶段 0-3：基线 / Workspace / SQLite+DPAPI / 核心业务 | 完成 | 第一/二/三轮验收报告 |
| 阶段 4-5：搜索/Embedding / MCP stdio | 完成 | 第三轮验收报告 |
| 阶段 6-7：Tauri/Vue 桌面 / Windows 能力与客户端注册 | 完成 | 第四轮验收报告 |
| 阶段 8：验收与发布（备份/日志轮换/路径治理/env Token 治理/NSIS+绿色版/性能测量/演练/文档） | 完成（工作集两项偏差待本机复核） | 第五轮验收报告（忆栈-Tauri迁移-第五轮验收报告.md） |

检查点 C 事项：发布门禁全绿（见 docs/发布清单-0.4.0.md）；工作集偏差与 Codex/WorkBuddy 实连确认转用户本机执行。

## 一、执行结论

本次迁移采用以下目标架构：

```text
Vue WebView
    ↓ Tauri Command
Tauri 桌面进程
    ↓
共享 Rust Application
    ↓
rusqlite + SQLite/FTS5

Codex / Claude / Cursor / WorkBuddy
    ↓ MCP stdio
独立的 MemStack-MCP.exe
    ↓
同一个 Rust Application 和 SQLite 数据库
```

最终交付两个职责独立的可执行程序：

```text
忆栈.exe             Tauri 桌面客户端
MemStack-MCP.exe     无界面的 MCP stdio 服务
```

桌面界面不再通过 `127.0.0.1:18461` 调用 REST，外部 AI 默认也不再通过 `127.0.0.1:10212/mcp` 调用 MCP。正式版本默认不监听任何本地 HTTP 端口。

## 二、不可变架构决策

迁移期间不得随意改变以下决策；如需改变，必须单独形成架构决策记录并重新评估范围。

1. Vue 页面通过 Tauri Command 调用 Rust，不保留桌面 REST 兼容层。
2. 外部 AI 通过标准 MCP stdio 调用，不直接调用 Tauri Command。
3. MCP stdio 使用独立轻量程序，不通过参数让 Tauri GUI 程序切换为 MCP 模式。
4. Tauri 与 MCP 只共享领域、业务和数据访问代码，不复制业务逻辑。
5. 数据库继续使用现有 SQLite 文件和 schema 7，不进行导出后重新导入。
6. 第一版 Rust 客户端不修改数据库结构版本，先实现对 schema 7 的完全兼容。
7. SQLite 必须保留 WAL、外键、5 秒锁等待、事务和 FTS5。
8. 现有记忆、项目、历史版本、候选、Token、Embedding 和后台任务数据必须无损保留。
9. Windows DPAPI 密文必须跨 C# 与 Rust 双向兼容，不能要求用户重新填写密钥。
10. 现有 16 个 MCP 工具的名称、参数、权限和返回结构保持兼容。
11. 桌面端负责消费 Embedding 后台任务；MCP 进程只提交任务，不并行启动多个后台消费者。
12. MCP stdout 只允许输出协议消息，日志只能写入 stderr 或日志文件。
13. 默认不实现 HTTP MCP；未来需要远程调用时，以独立传输适配程序增加，不混入 Tauri 主进程。

## 三、当前系统基线

### 3.1 当前模块

| 模块 | 当前职责 | 迁移目标 |
|---|---|---|
| `UnifiedAiMemory.Desktop` | WPF、WebView2、托盘、单实例、窗口状态 | `src-tauri` |
| `UnifiedAiMemory.Api` | REST、MCP HTTP/stdio、业务、SQLite、Embedding | Rust Workspace |
| `UnifiedAiMemory.Api.Tests` | C# 数据库和业务测试 | 作为迁移契约基线 |

### 3.2 已确认规模

- C# API 后端：21 个源码文件，约 5802 行。
- WPF 宿主：5 个源码文件，约 863 行。
- REST 接口：51 个。
- MCP 工具：16 个。
- SQLite：schema 7，包含普通表、索引和 FTS5 虚拟表。
- 后端测试：32 项。
- Vue 前端测试：14 项。
- 当前绿色版 EXE：约 85.81MB。
- 当前运行进程树实测工作集：约 48.28MB。

### 3.3 当前测试问题

迁移开始前必须先恢复全绿基线：

1. 两项测试仍断言 schema 版本为 6，生产代码已经升级到 7。
2. 两项 MCP 固定端口测试会在正式客户端占用 `10212` 时失败。
3. 当前结果为后端 28/32 通过，前端 14/14 通过。

基线修复不得改变生产业务，只修正测试预期和端口测试隔离方式。

## 四、目标代码结构

```text
desktop-client/
├── Cargo.toml
├── memory-desktop-web/
│   ├── src/
│   └── dist/
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── src/
│       ├── main.rs
│       ├── commands/
│       ├── lifecycle/
│       └── state.rs
└── rust/
    ├── crates/
    │   ├── memory-domain/
    │   ├── memory-application/
    │   ├── memory-storage/
    │   ├── memory-platform/
    │   └── memory-mcp/
    └── apps/
        └── memstack-mcp-stdio/
            ├── Cargo.toml
            └── src/main.rs
```

### 4.1 `memory-domain`

只包含稳定的数据和错误契约：

- 记忆、项目、候选、历史版本实体。
- MCP 客户端、权限、状态和调用上下文。
- 搜索、上下文、Embedding DTO。
- 业务错误码和错误消息。
- `serde` 序列化规则。

该模块禁止依赖 Tauri、MCP SDK、SQLite、Windows API 和 HTTP 客户端。

### 4.2 `memory-application`

实现业务用例：

- `MemoryService`
- `ProjectService`
- `WorkspaceService`
- `MemoryCandidateService`
- `SearchService`
- `EmbeddingService`
- `McpAccessService`
- `OverviewService`

Tauri Command 与 MCP 工具只能调用该层，不得直接拼接 SQL。

### 4.3 `memory-storage`

负责：

- `rusqlite` 连接和事务。
- schema 识别和版本迁移。
- SQL Repository。
- FTS5 写入与查询。
- WAL、外键和锁等待配置。
- 数据库兼容性检查。
- 跨进程迁移锁。

### 4.4 `memory-platform`

负责 Windows 平台能力：

- `%LOCALAPPDATA%` 路径解析。
- DPAPI 加密和解密。
- 单实例锁和窗口唤醒。
- 日志目录。
- WebView2 用户数据目录。
- 安装路径和 MCP 程序路径解析。

### 4.5 `memory-mcp`

负责：

- 16 个 MCP 工具定义。
- MCP 调用上下文。
- 只读和读写权限。
- 项目范围校验。
- 来源名称注入。
- 工具参数和返回结构映射。

该模块不负责进程启动、Tauri 窗口和数据库迁移。

## 五、桌面 Command 设计

现有 REST 接口按业务用例转换为单一职责 Command，不一比一保留 HTTP 路径。

### 5.1 总览

```text
get_overview
```

### 5.2 项目

```text
list_projects
create_project
update_project
archive_project
restore_project
bind_project_workspace
unbind_project_workspace
```

### 5.3 记忆

```text
list_memories
get_memory
create_memory
quick_capture_memory
update_memory
archive_memory
restore_memory
delete_memory_permanently
list_memory_revisions
restore_memory_revision
get_memory_facets
```

### 5.4 候选

```text
list_memory_candidates
update_memory_candidate
confirm_memory_candidate
reject_memory_candidate
```

### 5.5 工作空间

```text
resolve_workspace
store_workspace_memory
```

### 5.6 检索

```text
search_memories
build_memory_context
```

### 5.7 Embedding

```text
get_embedding_settings
test_embedding_settings
save_embedding_settings
rebuild_embeddings
get_embedding_status
```

### 5.8 MCP 客户端管理

```text
list_mcp_clients
get_mcp_client
create_mcp_client
update_mcp_client
rotate_mcp_client
revoke_mcp_client
delete_mcp_client
reveal_mcp_client_secret
test_mcp_client
register_mcp_client_config
check_mcp_client_connection
```

删除以下仅服务 HTTP 架构的能力：

```text
MCP 固定端口状态
MCP 端口重启
18461 桌面访问密钥
URL fragment 中的 desktopKey
REST 与 MCP 端口隔离检查
```

所有 Command 返回统一结果：成功时返回 DTO，失败时返回稳定的业务错误对象。错误对象至少包含 `code`、`message` 和 `details`，前端继续按错误码执行恢复操作。

## 六、SQLite 与数据兼容计划

### 6.1 编译要求

- 使用 `rusqlite`。
- SQLite 必须静态包含 FTS5。
- 启动测试必须检查 `PRAGMA compile_options` 包含 `ENABLE_FTS5`。
- Windows x64 使用小端浮点格式。

### 6.2 连接要求

每次打开连接后显式执行：

```sql
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
```

数据库初始化显式执行：

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
```

桌面进程初始连接池上限设为 4；每个 MCP stdio 进程连接池上限设为 2。所有同步数据库操作进入专用阻塞执行层，不占用 Tauri 异步运行时线程。

### 6.3 多进程迁移保护

桌面进程和多个 MCP 进程都可能首先打开数据库。迁移前必须取得 Windows CurrentUser 范围的命名互斥锁：

```text
Local\UnifiedAiMemory.Database.Migration
```

取得锁后重新读取 `PRAGMA user_version`，确认仍需要迁移后再执行事务。未取得锁的进程等待迁移结束，不允许自行并行执行 `ALTER TABLE`。

### 6.4 格式兼容

必须建立以下兼容规则：

- GUID 使用标准小写或大小写不敏感的带连字符字符串。
- 时间继续使用可排序的 UTC ISO 8601 文本，精度和时区格式固定。
- Rust 能解析 C# `DateTimeOffset.ToString("O")` 产生的 7 位小数格式。
- JSON DTO 使用 camelCase 字段。
- `Personal`、`Project`、`Active`、`Archived` 等枚举值保持现有大小写。
- `keywords_json`、`tags_json`、`aliases_json` 保持 JSON 数组格式。
- `vector_blob` 使用小端 `f32` 编解码。
- 内容校验和继续使用当前 SHA-256 规则。
- Cursor 编码和排序顺序保持稳定。

### 6.5 数据库样本

测试资源中至少保留：

1. 全新空数据库。
2. schema 1 数据库。
3. schema 2 至 schema 6 数据库。
4. 当前 schema 7 数据库。
5. 带项目、记忆、候选、历史版本和 FTS 数据的数据库。
6. 带 Embedding BLOB 的数据库。
7. 带 MCP 会话和 DPAPI 密文的数据库。
8. 旧 Java 同名数据库，用于验证自动切换到 `desktop-memory.db`。

第一版 Rust 发布不得把 schema 7 自动提升到新版本。只有完全通过兼容验收后，后续功能才允许增加 schema 8。

## 七、DPAPI 与凭据计划

### 7.1 兼容目标

Rust 使用 Windows `CryptProtectData` 和 `CryptUnprotectData`，保持：

```text
DataProtectionScope.CurrentUser
Entropy = UTF-8("UnifiedAiMemory.Desktop.0.3.0")
```

### 7.2 必测场景

- C# 加密，Rust 解密。
- Rust 加密，C# 解密。
- 原有 MCP Token 可查看、轮换和吊销。
- 原有 Embedding API Key 可读取和调用。
- 不同 Windows 用户不能解密。
- 无效 Base64、损坏密文和 DPAPI 失败返回稳定错误码。
- 日志、panic、stderr 和前端错误中不得出现明文密钥。

### 7.3 stdio 客户端身份

推荐配置只保存客户端会话标识，不把明文 Token 写入 Codex 或 WorkBuddy 配置：

```text
MemStack-MCP.exe --session-id <客户端会话GUID>
```

MCP 程序根据 `session_id` 从 SQLite 读取有效客户端和 DPAPI 密文，完成吊销、过期、权限和项目范围校验。同一 Windows 用户是本地安全边界。

如保留环境变量 Token 兼容，必须单独形成兼容期限和删除计划，不能长期维护两套身份解析逻辑。

## 八、MCP stdio 实施计划

### 8.1 进程规则

- 每个 AI 客户端连接启动一个 `MemStack-MCP.exe`。
- 程序不创建窗口、托盘和 HTTP 监听。
- stdin EOF 后正常退出。
- stdout 只写 MCP 帧。
- stderr 只写不含敏感信息的故障摘要。
- 详细日志写入 `%LOCALAPPDATA%\UnifiedAiMemory\logs\mcp-stdio.log`。
- 桌面程序未运行时，MCP 程序仍能独立访问数据库。

### 8.2 必须保留的 16 个工具

读取工具：

```text
memory_search
memory_get
memory_recent
memory_context
memory_related
```

正式记忆工具：

```text
memory_create
memory_update
memory_archive
```

候选工具：

```text
memory_candidate_submit
memory_candidate_list
memory_candidate_confirm
memory_candidate_reject
```

项目工具：

```text
project_list
project_resolve
project_create
project_update
```

### 8.3 权限要求

- 只读客户端不能调用写工具。
- 读写客户端可以调用全部授权工具。
- 项目范围客户端只能访问绑定项目。
- 记忆来源始终取客户端会话显示名。
- MCP 参数不能覆盖来源名称。
- 候选确认前不能进入正式记忆、FTS、Embedding 和最近记忆。
- 客户端吊销或过期后，新 MCP 初始化必须失败。

### 8.4 连接状态

客户端会话状态使用以下状态机：

```text
未配置
已配置，等待客户端启动
正在连接
已连接
启动失败
配置失效
已吊销
已过期
```

MCP 初始化成功时更新 `last_seen_at`；每次工具调用更新 `last_used_at` 和 `call_count`。桌面页面只有收到实际初始化记录后才能显示“已连接”。

## 九、AI 客户端注册计划

### 9.1 注册流程

“连接 AI”页面执行：

1. 创建客户端会话。
2. 生成稳定的 session ID。
3. 检查 `MemStack-MCP.exe` 绝对路径。
4. 根据客户端类型生成配置。
5. 用户确认后写入对应客户端配置。
6. 修改前创建配置备份。
7. 启动独立测试进程，发送 `initialize` 和 `tools/list`。
8. 提示用户重启目标 AI 客户端。
9. 根据真实 `last_seen_at` 显示连接结果。

### 9.2 Codex

写入 `~/.codex/config.toml` 的独立配置段，或通过 Codex MCP 管理命令注册。必须使用 TOML 解析器，不允许使用字符串拼接修改配置。

示例：

```toml
[mcp_servers.unified_ai_memory]
command = "C:\\Users\\z\\AppData\\Local\\Programs\\MemStack\\MemStack-MCP.exe"
args = ["--session-id", "<客户端会话GUID>"]
enabled = true
required = false
startup_timeout_sec = 10
tool_timeout_sec = 60
```

`required` 第一版明确设置为 `false`，避免记忆工具故障阻止 Codex 主任务启动；连接失败通过 Codex MCP 状态和忆栈页面展示。

### 9.3 WorkBuddy 与其他客户端

为已知客户端建立配置适配器，每个适配器只负责：

- 检测客户端是否安装。
- 返回配置路径。
- 生成 stdio 配置。
- 结构化写入或输出可复制配置。
- 验证配置是否包含当前 MCP 程序绝对路径和 session ID。

未知客户端使用通用 stdio 配置生成器。客户端若不支持 `command`、`args` 和 stdio，则明确显示“不支持 stdio MCP”，不得伪装成已连接。

### 9.4 安装路径

自动注册要求 MCP EXE 位于稳定路径：

```text
%LOCALAPPDATA%\Programs\MemStack\MemStack-MCP.exe
```

安装版作为推荐交付方式。绿色版允许注册当前绝对路径，但必须提示移动目录后配置会失效，并提供“一键重新注册”。

## 十、Embedding 与后台任务计划

### 10.1 所有权

- Tauri 桌面进程是唯一的 `background_task` 消费者。
- MCP stdio 进程只写入待处理任务。
- 多个 MCP 进程不得各自启动 Embedding Worker。
- 桌面程序关闭时，已排队任务保留在 SQLite。
- 下次启动桌面程序后继续处理。

### 10.2 HTTP 边界

取消本地 HTTP 服务不等于完全删除 HTTP 客户端。Embedding 仍通过 Rust HTTP 客户端访问 OpenAI 兼容地址。

必须保留：

- 30 秒请求超时。
- API Key DPAPI 保护。
- 模型维度校验。
- 查询向量缓存。
- 最多 3 次后台任务尝试。
- 指数退避。
- 失败记录数量限制。

## 十一、Tauri 桌面实施计划

### 11.1 启动顺序

```text
取得单实例锁
→ 解析 LocalAppData 路径
→ 取得数据库迁移锁
→ 初始化 schema 7
→ 启动 Embedding Worker
→ 注册 Tauri Commands
→ 创建主窗口和托盘
→ 加载 Vue 静态资源
```

不再等待两个 ASP.NET Core Host 启动，因此窗口展示不依赖本地端口。

### 11.2 桌面能力

必须迁移：

- Windows 单实例。
- 第二次启动唤醒现有窗口。
- 托盘打开和退出。
- 关闭窗口时隐藏到托盘。
- 窗口大小、位置和最大化状态恢复。
- 启动失败中文提示。
- 退出时停止后台任务并释放数据库连接。
- WebView2 缺失提示。

### 11.3 安全设置

- Tauri Capability 只开放实际使用的 Commands。
- Vue 不获得任意文件系统和 Shell 权限。
- Release 禁用开发者工具。
- 配置严格的 CSP。
- 不把数据库路径、Token 和 API Key注入前端。
- 不再使用 URL fragment 传递桌面管理密钥。
- WebView 用户数据放入 `%LOCALAPPDATA%\UnifiedAiMemory\webview`，禁止写入发布目录。

## 十二、Vue 改造计划

### 12.1 调用适配层

保留 `src/api.ts` 作为唯一调用入口，将 `fetch` 实现替换为 Tauri `invoke`。Vue 页面不得直接散落调用 `invoke`。

转换原则：

- TypeScript DTO 字段保持现有 camelCase。
- `AbortSignal` 改为页面侧忽略过期响应或使用请求标识。
- `ApiRequestError` 继续保留业务错误码。
- 页面、布局、交互和中文文案保持不变。
- 删除 `desktopAccessKey` 和 URL fragment 初始化。
- 删除本地端口健康检查。
- MCP 页面改为 stdio 配置与连接状态。

### 12.2 页面验收

- 总览。
- 记忆列表和筛选。
- 快速记录。
- 新建、编辑、归档、恢复和永久删除。
- 历史版本恢复。
- 项目和工作空间绑定。
- 候选确认和拒绝。
- 中文、英文和版本号检索。
- Embedding 设置和重建状态。
- 动态 AI 客户端创建、编辑、测试、轮换、吊销和删除。
- Token 雾化与敏感信息清理。

## 十三、分阶段执行计划

### 阶段 0：冻结 C# 行为基线，2 至 3 个工作日

任务：

1. 修复 schema 6/7 测试预期。
2. 隔离固定端口测试，确保正式客户端运行时测试也可执行。
3. 后端 32/32、前端 14/14 全部通过。
4. 保存 schema 1 至 7 测试数据库样本。
5. 导出 51 个 REST 接口和 16 个 MCP 工具契约。
6. 记录二万条数据检索 P95、启动时间、内存和发布体积。
7. 生成当前绿色版和 SHA-256，作为回滚版本。

验收：所有基线测试全绿，测试样本可在隔离目录重复运行。

### 阶段 1：建立 Rust Workspace，2 至 3 个工作日

任务：

1. 创建 Cargo Workspace 和六个职责模块。
2. 建立统一错误模型和中文错误码。
3. 配置格式化、Lint、单元测试和 Release Profile。
4. 建立 Tauri 空壳和 MCP stdio 空壳。
5. 验证两个程序可独立构建。

验收：Tauri 能加载 Vue，MCP 程序能在 stdin EOF 后干净退出。

### 阶段 2：SQLite 和 DPAPI 高风险验证，5 至 8 个工作日

任务：

1. 实现数据库路径识别和旧 Java 数据库保护。
2. 实现 schema 1 至 7 初始化和迁移。
3. 实现 FTS5 编译检查。
4. 实现跨进程迁移锁。
5. 实现 DPAPI 双向兼容。
6. 实现时间、GUID、JSON 和向量 BLOB 兼容测试。
7. 使用现有 schema 7 数据库进行只读和写入验证。

验收检查点 A：

- Rust 可直接打开现有生产数据库副本。
- C# 和 Rust 双向读写后数据一致。
- DPAPI 密文双向兼容。
- FTS5 中文查询可运行。

任一项无法实现时停止全量重写，继续保留 C# 架构。

### 阶段 3：核心业务迁移，8 至 12 个工作日

任务：

1. 迁移 ProjectService。
2. 迁移 WorkspaceService。
3. 迁移 MemoryService 和历史版本。
4. 迁移 MemoryCandidateService。
5. 迁移 McpAccessService。
6. 迁移 OverviewService。
7. 保持事务边界、错误码和乐观锁语义。
8. 使用相同数据库样本对比 C# 与 Rust 返回结果。

验收：项目、记忆、候选、版本、权限和工作空间契约测试全部通过。

### 阶段 4：搜索与 Embedding，5 至 8 个工作日

任务：

1. 迁移中文单字和二元分词。
2. 迁移 FTS5 MATCH 表达式和 BM25 权重。
3. 迁移模糊回退。
4. 迁移 Weighted RRF、MMR 和解释原因。
5. 迁移向量归一化和点积。
6. 迁移 Embedding API 和缓存。
7. 迁移后台任务领取、重试和清理。
8. 复跑二万条数据性能测试。

验收：关键词检索二万条数据 P95 小于 100ms，搜索顺序和上下文预算与 C# 基线一致。

### 阶段 5：MCP stdio，5 至 8 个工作日

任务：

1. 实现 MCP initialize、tools/list 和 tools/call。
2. 注册现有 16 个工具。
3. 实现 session ID 身份加载。
4. 实现权限、项目范围和来源保护。
5. 实现 stdout 纯净检查。
6. 实现 EOF、取消、异常和退出码处理。
7. 使用 Codex 完成真实 stdio 连接。
8. 使用 WorkBuddy 或另一个支持 stdio 的客户端完成第二客户端验证。

验收检查点 B：

- Codex 能自动启动 MCP EXE。
- `tools/list` 精确返回 16 个工具。
- 读写、只读、项目范围、过期和吊销测试通过。
- 两个客户端同时运行时 SQLite 无锁错误和数据越权。

Rust MCP SDK 或实现若无法稳定兼容目标客户端，在此检查点停止桌面迁移。

### 阶段 6：Tauri Command 与 Vue 切换，6 至 10 个工作日

任务：

1. 实现全部 Command。
2. 改造 `api.ts`。
3. 移除桌面 REST 和 desktopKey。
4. 改造 MCP 连接页面。
5. 保持现有页面与交互行为。
6. 补充 Vue Mock 和 Command 集成测试。
7. 验证错误码恢复路径。

验收：所有现有页面功能通过，系统不监听 `18461`。

### 阶段 7：Windows 桌面能力和安装注册，4 至 6 个工作日

任务：

1. 单实例和窗口唤醒。
2. 托盘和退出流程。
3. 窗口状态恢复。
4. WebView2 用户目录迁移。
5. Codex 配置结构化注册。
6. WorkBuddy 和通用 stdio 配置生成。
7. 安装路径、升级和卸载清理。
8. 绿色版路径移动检测。

验收：重复启动、托盘隐藏、完整退出、客户端注册和卸载均不留下错误配置。

### 阶段 8：全量验收与发布，4 至 6 个工作日

任务：

1. 自动化测试全量执行。
2. 使用真实用户数据库副本升级。
3. 完成 Codex、WorkBuddy 和通用客户端实测。
4. 完成性能、内存和体积测量。
5. 生成安装版和绿色版。
6. 生成 SHA-256。
7. 编写用户升级说明、回滚说明和验收报告。

验收检查点 C：全部完成标准满足后，Rust 版本才替代 C# 正式版本。

## 十四、测试矩阵

### 14.1 单元测试

- 字段校验和规范化。
- 内容校验和。
- Cursor 编解码。
- 工作空间标识规范化。
- 中文分词。
- RRF、MMR 和向量计算。
- Token 权限和项目范围。
- 时间与 GUID 格式。

### 14.2 数据库集成测试

- schema 1 至 7 升级。
- 迁移中断后恢复。
- Java 数据库保护。
- WAL 和锁等待。
- 并发创建项目。
- 重复记忆约束。
- 历史版本保留。
- 候选确认事务。
- FTS 同步。
- Embedding BLOB。

### 14.3 跨语言契约测试

对同一数据库样本分别执行 C# 和 Rust 用例，对比：

- JSON 字段和枚举。
- 错误码。
- 查询排序。
- 数据库行结果。
- FTS 结果。
- DPAPI 密文。
- MCP 工具 Schema。

### 14.4 MCP 测试

- initialize。
- tools/list。
- 16 个 tools/call。
- stdin 分帧。
- stdout 纯净。
- stderr 日志。
- EOF 退出。
- 客户端中断。
- 吊销和过期。
- 五个 MCP 进程并发读写。
- Codex 真实启动。
- WorkBuddy 真实启动。

### 14.5 桌面测试

- 冷启动和热启动。
- 单实例唤醒。
- 托盘隐藏和恢复。
- 窗口状态。
- WebView2 缺失。
- 数据库损坏提示。
- DPAPI 失败提示。
- 安装、升级和卸载。
- 绿色版移动后的配置修复。

## 十五、性能和发布目标

以下目标都以 Windows x64 Release 构建和相同测试机器为准：

| 指标 | 目标 |
|---|---:|
| 两个 EXE + Vue 静态资源 | 不超过 35MB，不含系统 WebView2 Runtime |
| Rust 桌面主进程工作集 | 空闲不超过 15MB |
| 完整 WebView2 进程树工作集 | 空闲不超过 60MB |
| MCP stdio 单进程工作集 | 空闲不超过 12MB |
| 二万条关键词检索 | P95 小于 100ms |
| 普通本地 CRUD Command | P95 小于 50ms |
| 冷启动至窗口可交互 | P95 小于 800ms |
| 热启动至窗口可交互 | P95 小于 400ms |
| MCP initialize | P95 小于 500ms |

不使用“Rust 初始化小于 100ms”代替完整应用启动指标。启动验收必须测量窗口真正可交互时间。

## 十六、日志和诊断

日志目录：

```text
%LOCALAPPDATA%\UnifiedAiMemory\logs\
```

文件：

```text
desktop.log
mcp-stdio.log
database-migration.log
embedding-worker.log
```

日志要求：

- 包含时间、级别、模块、错误码和调用标识。
- 不记录 Token、API Key、请求正文和完整记忆内容。
- MCP stdout 不记录日志。
- 每个 MCP 进程记录 PID、客户端 session ID 和退出原因，但不记录明文凭据。
- 日志按大小轮换并限制总占用。

## 十七、数据备份与回滚

### 17.1 首次运行备份

Rust 版本第一次写入生产数据库前：

1. 检查数据库完整性。
2. 使用 SQLite Online Backup API 创建一致性备份。
3. 备份到 `%LOCALAPPDATA%\UnifiedAiMemory\backup\pre-rust-<时间>.db`。
4. 写入备份校验和。
5. 确认备份可打开后才允许继续。

禁止在数据库打开且 WAL 未处理时只复制主 `.db` 文件。

### 17.2 回滚条件

发生以下任一情况立即回滚：

- DPAPI 无法读取原凭据。
- 记忆或项目数量不一致。
- FTS 结果明显缺失。
- MCP 权限越权。
- 数据库迁移失败。
- 连续出现无法恢复的 SQLite 锁错误。
- Rust 版本无法稳定连接两个目标 AI 客户端。

### 17.3 回滚步骤

1. 退出 Tauri 和全部 MCP 进程。
2. 保留故障数据库和日志，不直接覆盖。
3. 恢复首次运行备份。
4. 恢复 C# 0.4.0 绿色版。
5. 恢复原 AI 客户端 MCP 配置备份。
6. 核对项目、记忆、候选和搜索。

在 Rust 第一版不改变 schema 7 的前提下，优先尝试直接切回 C#；只有数据库内容验证失败时才恢复备份。

## 十八、主要风险

| 风险 | 等级 | 缓解措施 |
|---|---|---|
| Rust MCP SDK 与客户端协议差异 | 高 | 阶段 5 前完成 Codex 和第二客户端真实验证 |
| DPAPI 跨语言不兼容 | 高 | 阶段 2 双向黄金测试 |
| SQLite 多进程迁移竞争 | 高 | Windows 命名互斥锁和事务后重读版本 |
| FTS5 未编译进 SQLite | 高 | 启动编译选项检查和构建测试 |
| C# 与 Rust 时间文本排序不同 | 高 | 固定 UTC 格式和数据库样本测试 |
| 16 个 MCP 工具 Schema 漂移 | 高 | tools/list 快照和跨语言契约测试 |
| Vue 从 fetch 切换 invoke 后错误语义变化 | 中 | 保留错误码并集中改造 api.ts |
| 不同 AI 客户端配置格式不同 | 中 | 每个已知客户端独立适配器 |
| 绿色版移动导致 command 路径失效 | 中 | 路径检测和一键重新注册 |
| 两个 Rust EXE 静态代码重复导致体积增加 | 中 | Release LTO、strip 和依赖审计 |
| WebView2 缓存污染发布目录 | 中 | 固定 LocalAppData 用户目录并清理输出 |

## 十九、继续或终止标准

### 检查点 A：数据层

只有 SQLite、FTS5、DPAPI 和 schema 7 全部兼容，才继续迁移业务。

### 检查点 B：MCP

只有 Codex 和第二个 stdio 客户端都能稳定自动启动 MCP，才继续替换桌面端。

### 检查点 C：发布

只有数据、功能、权限、性能、体积和回滚全部验收，才用 Rust 版本替代 C# 正式发布。

任何检查点失败时，不允许用临时 HTTP 服务、C# sidecar 或复制数据库绕过问题；应停止并重新评估架构。

## 二十、完成定义

项目满足以下所有条件后才视为迁移完成：

- C# 基线测试在迁移前全部通过。
- Rust 单元、集成、契约和端到端测试全部通过。
- Vue 全部测试通过。
- 现有 schema 7 数据库可直接使用。
- C# 与 Rust DPAPI 双向兼容。
- 16 个 MCP 工具完整可用。
- Codex 和 WorkBuddy 或第二个目标客户端能自动启动 stdio MCP。
- 多客户端并发不产生数据错误和权限越界。
- Vue 不再发送 `/api/*` 请求。
- 系统不监听 `18461` 和 `10212`。
- Tauri 桌面功能与当前 WPF 版本等价。
- 发布目录不包含 WebView2 用户缓存。
- 安装版和绿色版均可运行。
- 首次运行自动备份和回滚流程经过真实演练。
- 发布体积和性能达到目标或形成经批准的偏差记录。
- 用户升级说明、AI 客户端接入说明和故障排查文档齐全。

## 二十一、建议工期

以一名熟悉 Rust、Tauri、SQLite 和 MCP 的开发者全职执行估算：

| 阶段 | 预计工作日 |
|---|---:|
| 阶段 0：基线 | 2-3 |
| 阶段 1：Workspace | 2-3 |
| 阶段 2：SQLite/DPAPI | 5-8 |
| 阶段 3：核心业务 | 8-12 |
| 阶段 4：搜索/Embedding | 5-8 |
| 阶段 5：MCP stdio | 5-8 |
| 阶段 6：Tauri/Vue | 6-10 |
| 阶段 7：Windows/安装 | 4-6 |
| 阶段 8：验收/发布 | 4-6 |

总工期约 41 至 64 个工作日。若开发者正在学习 Rust 或 MCP，必须增加缓冲；不得通过减少数据兼容、权限和回滚测试压缩工期。

## 二十二、第一周立即执行事项

1. 生成 schema 1 至 7 数据库样本和 C# DPAPI 黄金数据。
2. 创建 Cargo Workspace 和空 Tauri/MCP 程序。
3. 验证 Rust SQLite 构建包含 FTS5。
4. 用 Rust 打开现有 schema 7 数据库副本。
5. 完成 C# 加密、Rust 解密的 DPAPI 最小验证。
6. 实现最小 `memory_search` stdio MCP，并用 Codex 真实启动。
7. 在第一周结束时完成检查点 A 的初步评审，决定是否进入全量迁移。
