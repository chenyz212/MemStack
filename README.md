# MemStack 桌面客户端

这是 MemStack 的 Windows 个人桌面客户端，当前整体版本为 `0.4.2`，纯 Tauri + Rust 架构。

## 下载

请从 [GitHub Releases](../../releases/latest) 下载最新版：

- `MemStack_<版本>_x64-setup.exe`：Windows 安装版，推荐普通用户使用。
- `MemStack-Portable-<版本>.zip`：绿色版，解压后直接运行 `MemStack.exe`。
- `SHA256SUMS.txt`：安装包与绿色版的 SHA-256 校验值。

应用依赖 Microsoft Edge WebView2 Runtime；Windows 11 默认已安装，Windows 10 用户如无法启动，请先安装 WebView2 Runtime。

## 架构

- `src-tauri`：Tauri 2 桌面壳——窗口、系统托盘、单实例、窗口状态持久化，前端经 `invoke` IPC 调用命令层（无 HTTP 监听端口）。
- `rust/`：Rust 服务与契约层。
  - `crates/memory-domain`：领域模型、错误码、游标与校验和。
  - `crates/memory-application`：记忆/项目/工作空间/候选/检索/MCP 接入等项目应用服务。
  - `crates/memory-storage`：SQLite/FTS5 存储与结构迁移。
  - `crates/memory-platform`：Windows 平台设施（DPAPI、路径、日志轮换、命名互斥、进程树监控）。
  - `crates/memory-mcp`：MCP 工具契约（常规工具权威快照 `contracts/mcp-tools-list.json` + 受控追加的工具契约）与权限守卫、分发。
  - `apps/memstack-mcp-stdio`：`MemStack-MCP.exe`——无窗口 MCP stdio 服务器，供 AI 客户端接入。
- `memory-web`：Vue 3 前端（总览、记忆、图谱、项目文档、连接 AI、设置）。

客户端不实现登录，也不迁移旧 PostgreSQL 数据。个人与项目记忆默认保存在当前 Windows 用户的 `%LOCALAPPDATA%\MemStack\data\memory.db`；如果检测到同名旧 Java 数据库，会完整保留旧文件并自动使用 `desktop-memory.db`。

## 本地构建

构建机需要 Node.js、Rust 工具链、`rustfmt` 与 `clippy`：

```powershell
Set-Location memory-web
npm ci
npm run build

Set-Location ..
memory-web\node_modules\.bin\tauri.cmd build
```

产物：`target\release\memstack-desktop.exe`（主程序）、`target\release\MemStack-MCP.exe`（MCP stdio 服务器）、`target\release\bundle\nsis\MemStack_0.4.2_x64-setup.exe`（NSIS 安装包）。

开发调试：`npm run dev`（memory-web）+ `cargo run`（src-tauri），或直接 `tauri.cmd dev`。

## MCP 与工作空间记忆

客户端内「连接 AI」页面采用动态 AI 工具卡片：用户可以填写任意 AI 工具名称（如 `Codex`、`Claude Desktop`、`Cursor`、`Trae`、`WorkBuddy` 或自定义名称），每个工具对应一个独立的客户端会话。AI 客户端通过 stdio 接入（无窗口、无 HTTP 监听）：

```powershell
MemStack-MCP.exe --session-id <guid>   # 会话 ID 在「连接 AI」页面创建会话后获得
```

兼容路径：环境变量 `MEMSTACK_TOKEN`（历史版本遗留，后续移除）。会话 Token 使用当前 Windows 用户的 DPAPI 加密保存，默认雾化显示。

MCP 提供 22 个单一职责工具：

- 记忆读取：`memory_search`、`memory_get`、`memory_recent`、`memory_context`、`memory_related`。
- 正式记忆写入：`memory_create`、`memory_update`、`memory_archive`。
- 候选审核：`memory_candidate_submit`、`memory_candidate_list`、`memory_candidate_confirm`、`memory_candidate_reject`。
- 项目管理：`project_list`、`project_resolve`、`project_create`、`project_update`。
- 项目文档与结论卡片：`project_document_handoff_get`、`project_document_draft_create`、`project_document_draft_update`、`project_document_batch_update`、`project_document_restore_previous`、`conclusion_card_candidate_submit`。

只读 Token 仅能调用读取类工具，读写 Token 可调用全部工具；项目范围 Token 只能访问绑定项目。MCP 写入来源严格取自 Token 显示名。候选确认前不会进入正式记忆、FTS、Embedding、最近记忆和搜索上下文。项目文档的批量更新与上一版恢复均走乐观锁（`expectedVersion` 不符整批拒绝）；恢复仅回退到唯一保留的上一版快照，不接收正文，恢复前的内容会保留为新的上一版快照以支持反向恢复。

项目文档工具读写工作空间 `.memstack/` 目录下的五份正式文档（CONTEXT / DECISIONS / CURRENT_STATUS / PROBLEMS / CHANGELOG）：草稿由 AI 生成并经用户在桌面端逐份审核批准后自动晋升为正式文档，数据库持有只读镜像并校验同步状态。

项目记忆使用工作空间根目录名称作为标识，例如 `mcp-ai-memory`。同一标识忽略大小写匹配，即使项目移动到其他绝对路径，也会继续归入原来的中文项目。AI 应优先从对话上下文显式传入工作空间标识（完整路径或目录名）。`project_resolve` 返回三态：`MAPPED`（已绑定项目，记忆存项目空间）；`PROJECT_NAME_REQUIRED`（工作空间存在但未绑定项目——AI 向用户询问一次中文项目名，用答复调用 `project_create` 完成绑定，记忆存新项目空间）；`UNBOUND`（无工作空间上下文的纯对话，记忆存个人空间，不询问项目名）。

### 进程生命周期

MCP 进程回收采用三层防线，避免误杀存活连接或残留僵尸进程：

1. **stdin EOF**：客户端关闭管道或崩溃时秒级退出；
2. **祖先进程监控**：默认每 30 秒轮询 Windows 进程树确认客户端祖先进程存活（跳过 cmd/powershell 等壳进程，PID+名称双校验防 PID 复用），客户端死亡即自行退出，存活期间永不误杀；
3. **空闲超时兜底**：祖先链不可解析时退化为空闲超时回收（默认 24 小时）。

相关环境变量：`MEMSTACK_MCP_IDLE_TIMEOUT_SECS`（正整数秒覆盖空闲超时；0 = 禁用）、`MEMSTACK_MCP_ANCESTOR_POLL_MS`（祖先进程轮询间隔毫秒数）。生效值启动时写入 `%LOCALAPPDATA%\MemStack\logs\mcp-stdio.log`。

## 生成绿色版

```powershell
# 在仓库根目录执行
cargo build --release
powershell -ExecutionPolicy Bypass -File scripts\package-portable.ps1 -Version "0.4.2"
```

生成结果位于 `MemStack-Portable-0.4.2\`（MemStack.exe + MemStack-MCP.exe + version.txt，含 SHA-256）。绿色版解压即用，复用系统 WebView2 Runtime，无需 Java、Node.js 或外部数据库。
