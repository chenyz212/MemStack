# 忆栈 Tauri + Rust 迁移：第四轮验收报告（阶段 6 + 7）

> 日期：2026-08-15
> 范围：《忆栈-Tauri迁移-第四轮执行计划.md》T1–T12 全部任务（阶段 6：Tauri Command 与 Vue 切换；阶段 7：Windows 桌面能力与客户端注册）。
> 结论：**验收通过**。三线回归全绿，Release 产物可用，端口实证为零监听；页面验收 11 项中 9 项已由自动化 + 实测覆盖，2 项交互手测留待用户日常使用确认（见 §4）。

## 一、Command 覆盖表（45 业务命令 + 1 诊断命令）

注册清单见 `src-tauri/src/main.rs` `generate_handler!`，与总计划 §5.1–5.8 逐条对应，全部为既有服务薄映射，无新业务逻辑。

| 分组 | 命令 | 数量 | 单测 |
|---|---|---|---|
| 项目 | list_projects / create_project / update_project / archive_project / restore_project / bind_project_workspace / unbind_project_workspace | 7 | project_lifecycle_covers_all_commands、update_missing_project_returns_not_found |
| 工作空间/总览 | resolve_workspace / store_workspace_memory / get_overview | 3 | workspaces.rs、state.rs 装配测试 |
| 记忆 | list_memories / get_memory / create_memory / quick_capture_memory / update_memory / archive_memory / restore_memory / delete_memory_permanently / list_memory_revisions / restore_memory_revision / get_memory_facets | 11 | memory_lifecycle_covers_all_commands、quick_capture_generates_title_and_defaults、stale_version_update_returns_conflict |
| 候选 | list_memory_candidates / update_memory_candidate / confirm_memory_candidate / reject_memory_candidate | 4 | candidate_lifecycle_update_confirm_reject、confirm_with_stale_version_returns_conflict、confirmed_candidate_keeps_original_source |
| 检索 | search_memories / build_memory_context | 2 | search.rs 命令测试（复用第三轮 FTS/RRF/MMR 测试基建） |
| Embedding | get_embedding_settings / test_embedding_settings / save_embedding_settings / rebuild_embeddings / get_embedding_status | 5 | embedding.rs（掩码/保存/未配置错误码） |
| MCP 客户端 | list_mcp_clients / get_mcp_client / create_mcp_client / update_mcp_client / rotate_mcp_client / revoke_mcp_client / delete_mcp_client / reveal_mcp_client_secret / check_mcp_client_connection / test_mcp_client / get_mcp_connection / get_mcp_client_config_preview / register_mcp_client_config | 13 | mcp_client_lifecycle_covers_all_commands、test_mcp_client_reports_failure_when_exe_missing、test_mcp_client_handshakes_with_fake_exe、get_mcp_connection_reports_stdio_shape |
| 诊断 | get_app_info | 1 | （第一轮既有） |

要点：

- **错误模型 1:1 对齐 C#**：`CommandError{code,message,details}`，`details` 恒为空对象（C# `DesktopApiHost` 实测形状），快照测试逐字校验错误码与中文消息。
- **桌面固定 caller**：`AppState::desktop_caller()` = `(Guid.Empty, "桌面客户端", ReadWrite, null)`，与 C# `ApiEndpoints.cs:107` 一致，专项测试锁定。
- **revoke_mcp_client** 无独立前端路由：UI「删除」按钮语义为吊销+删除（C# 同为 DELETE 一体），`revoke_mcp_client` 保留为服务层能力，无 REST-only 遗留。

## 二、Vue 切换（T8/T9）

- `memory-desktop-web/src/api.ts`：`apiFetch(path, init, signal)` 签名不变，内部 40 条路由表（`METHOD + 路径模式 → {command, argsBuilder}`）转 `@tauri-apps/api/core` invoke；App.vue 除 MCP 接入区块外零改动。
- 错误传播：invoke reject 对象取 `code/message` 抛 `ApiRequestError`；非对象兜底 `REQUEST_FAILED`（对齐现网）。
- `initializeDesktopAccess`、desktopKey header、sessionStorage、端口健康检查全部删除；`AbortSignal` 参数保留但忽略，防抖/覆盖逻辑未破坏。
- MCP 页面：卡片展示 stdio 接入配置（command/args/完整配置预览）、「写入配置」（备份→结构化写入→验证，写前 confirm）、「复制配置」、「连接测试」；10212/端口语义区块移除。
- vitest：`api.test.ts` mock invoke 断言路由映射/参数组装/错误传播，22/22 全绿；`npm run build`（vue-tsc + vite）通过。

## 三、桌面能力（T10/T11）

| 项 | 实现 | 证据 |
|---|---|---|
| 单实例 | `tauri-plugin-single-instance` 最先注册；二次启动唤醒主窗口后退出 | `desktop_lifecycle.rs::second_instance_exits_zero_and_first_survives`：第二实例退出码 0、首实例存活 |
| 托盘 | 菜单「打开忆栈/退出」+ 左键单击恢复 + tooltip「忆栈」 | 集成测试运行期人工可见 + 代码路径 |
| 关闭到托盘 | CloseRequested → prevent_close + 保存状态 + hide（保留 WebView 进程零延迟恢复） | main.rs on_window_event |
| 窗口状态 | `%LOCALAPPDATA%\UnifiedAiMemory\window-state.json`，camelCase 五字段与 C# 同文件同格式；min 1080×720 校验；最大化恢复 | window_state.rs 5 项单测（含直接读取 C# 写出的 JSON、非法 JSON 容错） |
| WebView2 数据目录 | Rust setup 创建主窗口，data_directory 固定 `%LOCALAPPDATA%\UnifiedAiMemory\webview`；tauri.conf.json 静态 windows 段移除 | tauri.conf.json + create_main_window |
| CSP | `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src ipc: http://ipc.localhost` | tauri.conf.json；构建产物加载正常（Release 实测启动） |
| 退出流程 | 隐藏窗口+移除托盘 → 停 Worker → 保存窗口状态 → exit(0)；5s 退出保护线程兜底（对齐 C# ExitGuard） | exit_app + Worker 启停幂等测试 |
| 启动失败 | 中文摘要写 `logs/startup-error.log` + 原生 MessageBox 弹窗；成功启动后清理残留 | `desktop_lifecycle.rs::startup_failure_writes_chinese_log_and_blocks_on_message_box` 实测弹窗（用户可见确认），日志含「打开数据库失败」+ UTC 时间戳 |
| 端口 | **不监听 18461 与 10212** | netstat 实证：Release 进程运行期间 `netstat -ano | findstr "18461 10212"` 为空，且该进程零任何端口监听；集成测试断言两端口可绑定 |

## 四、页面验收记录（总计划 §12.2，11 项）

| # | 项 | 结果 |
|---|---|---|
| 1 | 总览 | 自动化：get_overview 命令测试通过；页面加载见 Release 实测启动 |
| 2 | 记忆列表筛选 | 自动化：list_memories 全参数路由测试（scope/status/favorite/pinned/type/tag/importanceMin/cursor） |
| 3 | 快速记录 | 自动化：quick_capture 标题生成/默认值测试 |
| 4 | 新建/编辑/归档/恢复/永久删除 | 自动化：memory_lifecycle 全链路（永久删除需先归档，语义对齐 C#） |
| 5 | 历史版本恢复 | 自动化：list_revisions + restore_revision 测试 |
| 6 | 项目与工作空间绑定 | 自动化：bind/unbind（标识归一化取末段对齐 C# WorkspaceIdentity） |
| 7 | 候选确认/拒绝 | 自动化：候选全生命周期 + 乐观锁冲突码 |
| 8 | 中英文/版本号检索 | 自动化：复用第三轮 search_perf（P95 中文 32ms/英文 67ms/混合 25ms）与命令层测试 |
| 9 | Embedding 设置与重建状态 | 自动化：设置读取/掩码/保存/重建/未配置错误码五命令测试 + Worker 启停幂等 |
| 10 | AI 客户端全生命周期（创建/编辑/测试/轮换/吊销/删除） | 自动化：mcp_client_lifecycle 全链路 + 假 exe 真握手（16 工具）+ exe 缺失中文报错 |
| 11 | Token 雾化与敏感清理 | 自动化：reveal 仅生成/轮换后返回明文；吊销后 reveal 拒绝（MCP_TOKEN_REVOKED） |

交互层手测（窗口拖动/最大化恢复/托盘行为/关闭到托盘后零延迟恢复）已由集成测试与 Release 启动覆盖主路径；**建议用户日常使用中确认体验无异常**（尤其用户此前关注的「托盘点击进入应用流畅无卡顿」——关闭到托盘保留了 WebView 进程，恢复路径与 C# 语义一致）。

## 五、注册演示证据（阶段 7 任务 5/6）

`rust/crates/memory-application/src/client_registration.rs`（9 项测试，全绿）：

- `codex_register_preserves_existing_sections_and_comments`：`~/.codex/config.toml` TOML 结构化编辑（toml crate，无字符串拼接），既有段与注释保留，写入前时间戳备份。
- `codex_register_creates_missing_config_without_backup` / `codex_register_rejects_invalid_toml`：缺失文件创建、非法 TOML 拒写。
- `claude_register_preserves_unknown_keys` / `cursor_register_is_idempotent_with_backup_each_time`：Claude/Cursor JSON 结构化写入 + 幂等 + 每次备份。
- `generic_clients_are_preview_only_without_writes` / `undetected_json_clients_report_missing_message_without_writes`：未知客户端仅生成可复制配置文本、不落盘、不伪装已连接（§9.3）。
- `client_key_normalization_maps_supported_clients` / `empty_outline_is_rejected`：客户端识别归一化与空大纲拒绝。

Commands：`register_mcp_client_config`（写入）+ `get_mcp_client_config_preview`（预览），App.vue「写入配置」前 confirm 展示目标路径与备份路径。

## 六、回归结果（T12）

| 检查 | 结果 |
|---|---|
| `cargo fmt --all --check` | 通过（0 diff） |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过（44 处 `Ok(x?)` 冗余已由 `cargo clippy --fix` 清理，含 window_state.rs 1 处） |
| `cargo test --workspace` | **219 通过 / 0 失败**（memory-application 104 + 契约 1 + search_perf 1 + domain 20 + mcp 11 + platform 8+2 + storage 6+5+1 + desktop 31 单测 + 2 生命周期集成 + stdio e2e/并发等） |
| `npm run test`（vitest） | 22/22 通过（3 文件） |
| `npm run build`（vue-tsc + vite） | 通过（dist 产物 247.43 kB js / 59.74 kB css） |
| `dotnet build -c Release` | 0 警告 0 错误（C# 本轮零改动） |
| `dotnet test` | 34/34 通过 |
| `cargo build --release -p memstack-desktop` | 通过（LTO thin，4m25s），`target/release/memstack-desktop.exe` 可运行（netstat 实测即用该产物） |
| `netstat -ano \| findstr "18461 10212"` | **为空**（进程运行期间实测） |

## 七、偏差记录

1. **tauri-plugin-dialog 未采用**：计划 T11 原定用该插件做启动失败弹窗；实际数据库装配失败发生在 `tauri::Builder` 构建之前，插件尚不可用，改用 Win32 `MessageBoxW` 原生弹窗（行为与 C# 完全一致：中文标题「忆栈」+ 摘要 + 日志路径）。依赖未引入，属简化偏差。
2. **假 exe 连接测试不回填 call_count**：`test_mcp_client` 握手成功但假 exe 不访问数据库，连接活动（callCount/lastSeenAt）回填由真实 stdio 服务认证时写入，已有 mcp_stdio_e2e 覆盖；命令层测试断言改为 0，语义更精确。
3. **候选确认 created_source**：首轮命令测试误写「桌面客户端」；领域语义（C# 与 Rust 服务层一致）为记录候选来源（如 Codex），已修正测试并加注释锁定。
4. **环境事件**：E 盘满（os error 112）曾导致 rustc 无诊断崩溃，用户清理后恢复；期间 `cargo clean -p memstack-desktop` 重编译，非代码问题。
5. **开发工作流**：统一 `tauri dev`（vite dev 由 Tauri 拉起），无浏览器直开双模式（按计划执行，非偏差，此处备查）。
6. **后补修复（2026-08-15 用户实测反馈）**：弹窗内「接入配置」tab 按钮只切换 tab 不触发预览加载，从「编辑 AI 工具」切入时显示误导提示「请先生成或轮换令牌后再查看接入配置」。修复：App.vue 新增 `switchMcpDialogTab`（切入 config 且无预览时按需加载，stdio 配置不依赖明文令牌）；提示文案改为中性表述。vitest 22/22 + vue-tsc/build + Release 重建全绿。
7. **后补改进二批（2026-08-15 用户实测反馈，三项）**：
   - **总览过滤归档项目**：`overview_service.rs` 最近记忆改为直接 SQL（复用 `SELECT_MEMORY_SQL`），排除已归档项目的记忆（C# 基线原样保留，属有意行为差异；记忆不删除，项目恢复后重新可见；`memory_count` 与记忆页「全部」口径一致不过滤）。新增测试 `recent_memories_exclude_archived_project_memories`；`OverviewService` 构造函数移除未用的 `memories` 参数。
   - **配置文本只含 MCP 段**：`client_registration.rs` 预览/注册报告的 `configText` 改为仅输出 MCP 段片段（Codex：`[mcp_servers.unified_ai_memory]` 段；Claude/Cursor/Generic：`{"mcpServers":{...}}`），不再搬出用户完整配置；注册落盘仍写完整合并文件（用户段落保留）。新增 `codex_preview_and_report_show_only_mcp_section` 测试与 Claude 片段断言。
   - **访问令牌区块 → 会话 ID**：App.vue 接入配置 tab 的「访问令牌」区块改为「会话 ID（stdio 接入标识）」（显示 sessionId + 复制）；删除 `mcpClientSecrets` 状态与 `revealMcpClientSecret`/`copyMcpClientToken`（明文令牌仅在创建/轮换后一次性展示）。令牌体系保留在编辑 tab（轮换/删除/有效期语义由 Token 驱动）。
   回归：cargo fmt/clippy/-workspace 221 项、vitest 22、vue-tsc/build、Release 重建全绿。

## 八、遗留与下一步

- 阶段 8（安装打包/分发策略）未启动：本轮 webviewInstallMode 保持 skip（绿色版理念），安装版策略届时另计划。
- 用户日常使用确认：托盘恢复流畅度、窗口位置跨迁移无缝（C# 旧 window-state.json 直接可读）。
- 下轮建议：按总计划进入阶段 8，或先行收集 11 项页面手测反馈。
