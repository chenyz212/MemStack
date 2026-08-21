# 第一轮验收报告（检查点 A 初评）

> 依据：`docs/忆栈-Tauri-Rust-迁移执行计划.md` §十九、`.trae/documents/忆栈-Tauri迁移-第一轮执行计划.md`
> 验收日期：2026-08-15
> 环境：Windows 11 x64 · Rust 1.97.1（stable-x86_64-pc-windows-msvc，清华镜像）· dotnet 10.0.400（项目内置）· VS Build Tools 2022

## 一、基线结果

| 项目 | 结果 |
|---|---|
| C# 后端测试 | 33/33 通过（原 32 + 新增 DPAPI 反向契约测试 1） |
| 前端 Vitest | 14/14 通过（3 个测试文件） |
| Rust 工作区 | 37/37 通过；`cargo fmt --check`、`cargo clippy -D warnings` 全绿 |
| 数据库样本 | `testdata/db-samples/`：v1→v7 逐版本快照、full-sample、legacy-java、empty、seed-20k（2 万条，43MB） |
| DPAPI 黄金数据 | `testdata/golden/dpapi-golden.json`（C# 加密）+ `rust-dpapi-roundtrip.json`（Rust 加密）+ `sample-token.json` |
| 契约快照 | `contracts/`：rest-endpoints.json、mcp-initialize.json、mcp-tools-list.json（16 工具） |
| 性能基线 | `docs/迁移基线-性能数据-0.4.0.md`（检索 P95：中文 21ms / 英文 91ms / 混合 18ms） |
| 回滚版本 | `E:\AICoding\mcp-ai-memory\dist\windows\统一AI记忆-Portable-0.4.0-x64\`，SHA-256 `E16BC08ECE90F8E2A1FCD85409DA7A20B0C717D75DB5B914AF19736ECB8F1881`（85.81MB） |
| Rust 产物 | `memstack-desktop.exe`（Tauri 空壳）7.85MB · `MemStack-MCP.exe`（stdio MCP）2.12MB（LTO thin + strip） |

## 二、高风险验证结论表

| 高风险点 | 结论 | 证据 |
|---|---|---|
| rusqlite 静态编译含 FTS5 | **通过**（无需绕过） | bundled SQLite `PRAGMA compile_options` 含 `ENABLE_FTS5`，`verify_fts5` 每次打开断言；测试 `bundled_sqlite_enables_fts5` |
| Rust 打开 schema 7 生产库副本 | **通过** | `local_production_copy` 2/2：只读校验（user_version=7 + 表计数）+ 事务写入冒烟 + WAL checkpoint 无锁错误；`full-sample.db` 中文 FTS MATCH 命中；GUID / 7 位小数 ISO-8601 / keywords_json / 小端 f32 向量解码全部通过 |
| schema 1-7 链 + 旧库回退 | **通过** | `opens_all_schema_chain_snapshots`（v1→v7 逐版本 user_version 断言）；`legacy_java_database_triggers_desktop_fallback`（memory.db 非桌面结构 → desktop-memory.db 回退） |
| DPAPI C#→Rust 解密 | **通过** | `dpapi_golden` 读取 C# 黄金数据逐条解密成功（ASCII/中文/特殊字符/空串） |
| DPAPI Rust→C# 解密 | **通过** | Rust `dpapi_roundtrip_generate` 生成密文 → C# `DpApi_RustCiphertextCanBeDecryptedByCSharp`（dotnet 33/33 内）解密比对一致，双向闭环 |
| MCP SDK 选型 | **采用手写 JSON-RPC 帧循环**（非 rmcp） | rmcp 0.8 transport-io 在最小集成路径上对 stdout 纯净性与初始化时序的控制粒度不足，调试成本高于收益；手写帧循环约 150 行（initialize/tools/list/tools/call + notifications），stdout 逐行 JSON 校验、日志容错（写文件失败静默降级 stderr）、EOF 干净退出全部由自有测试覆盖。该结论供阶段 5 检查点 B 参考：官方 SDK 并非必需，但完整工具集阶段应重新评估 |
| MCP stdio 端到端 | **通过** | e2e 4/4：完整会话（initialize→tools/list 精确 memory_search→tools/call 中文检索命中"记忆栈迁移基线"）、无效 Token 非 0 退出、未知工具 JSON-RPC 错误不崩、流式 stdout 纯净性 |
| TraeWork 真实验证（替代 Codex） | **通过** | TRAE 客户端（设置→MCP→手动添加）真实拉起 release 版 MemStack-MCP.exe，`memory_search` 中文检索"迁移基线"返回「记忆栈迁移基线」（score 1.0，KEYWORD 命中，schema 校验通过）。首次调用暴露 1 项契约偏差（见现网缺陷 2）并已修复后复验 |

## 三、现网缺陷发现（本轮重要产出）

**C# 0.4.0 客户端启动崩溃缺陷**（本轮实测触发并定位）：

- 位置：`MemoryDatabase.IsDesktopSchemaAsync`（`Mode=ReadOnly`）
- 机理：旧 Java `memory.db` 为 WAL 模式。只读连接打开 WAL 库要求 `-shm` 有效：残留陈旧 `-shm`（进程强杀后）→ `SQLite Error 10: disk I/O error`；`-shm` 不存在（Java 端正常关闭后）→ `SQLite Error 14: unable to open database file`。两种状态均使探测抛异常且 `ResolveDesktopDatabasePathAsync` 无捕获，客户端无法启动
- 本机处置：生产 `memory.db` 已转换 `journal_mode=DELETE`（数据不变，integrity ok；备份 `%TEMP%\uam-wal-backup-20260815-005323`）
- Rust 侧根治：`memory-storage` 结构探测改用 `immutable=1` URI 只读打开（不依赖 wal/shm），新增 2 项回归测试复现两种无 shm 场景（`schema_probe_opens_wal_database_without_shm_files` / `schema_probe_rejects_legacy_wal_database_without_shm_files`）
- C# 侧建议（阶段 3 一并处理）：探测连接加 `Immutable` 或捕获异常按回退处理；生产代码修复不在第一轮范围

**Rust MCP structuredContent 契约偏差**（TraeWork 真实调用暴露，已修复）：

- 现象：首次 TraeWork 调用 `memory_search` 被客户端按 outputSchema 校验拒绝（`structuredContent` 期望 record，实际返回裸数组）
- 根因：C# 契约 `outputSchema` 声明 `{"result": [...]}` 对象包装，而 Rust 初版把数组直接放进 `structuredContent`
- 修复：`tools_call` 返回值改为 `{"result": [...]}` 包装，e2e 断言同步对齐契约，重建 release 后 TraeWork 复验通过
- 价值证明：真实客户端严格校验比自测更能暴露契约偏差——阶段 5 完整 16 工具必须逐个对照 outputSchema 做快照测试

## 四、偏差记录

| # | 偏差 | 说明与补救 |
|---|---|---|
| 1 | ~~T4 冷启动/内存测量未在 AI 终端完成~~ **已解决** | 用户本机执行 `run-startup-baseline.ps1`：冷启动 P95=1975ms（5 次全 UP），空闲内存 709.6MB/7 进程。已回填 `docs/迁移基线-性能数据-0.4.0.md` |
| 2 | Tauri 空壳开窗验证未在 AI 终端执行 | WebView2 运行时数据目录写入受沙箱限制。用户可双击 `target\release\memstack-desktop.exe` 人工确认开窗渲染 Vue（REST 请求失败属预期） |
| 3 | ~~Codex 真实验证未在 AI 终端完成~~ **已解决（替代方案）** | 经用户确认改用 TraeWork 客户端做真实验证（语义等价）：TRAE 真实拉起 release 版 exe 并完成 `memory_search` 调用，**通过**（见结论表）。Codex 配置已恢复原样，`run-codex-verify.ps1` 留档不再使用 |
| 4 | dotnet 全量测试首次 32/33 | `Mcp_OccupiedFixedPortFailsAndRestartsAfterRelease` 在沙箱下偶发（该测试拉起真实 MCP 子进程写生产日志目录，被沙箱拦截干扰时序）；单独运行与全量重跑均 33/33 通过。非代码问题 |
| 5 | 新增 C# DPAPI 反向契约测试曾有编译错误（`await using JsonDocument`） | 已修复为 `using`；该错误说明上一会话结束时该测试从未编译通过，本轮修正后 33/33 成立 |
| 6 | E 盘清理导致 Rust 工具链丢失 | 已重装（rustup 1.97.1 minimal + rustfmt + clippy，安装于 C 盘用户目录避免再次误伤）；rust\target 缓存丢失导致全量重编译，无代码损失 |
| 7 | e2e 测试并发缺陷（本轮发现并修复） | 4 个 stdio e2e 共用仅按 pid 命名的临时目录，并行互删数据库副本；已加线程 id 隔离 |
| 8 | REST 契约采用路由表静态解析而非 OpenAPI | `contracts/rest-endpoints.json`（保底方案，路由信息 100% 来自 `ApiEndpoints.cs`）；OpenAPI 导出留待阶段 3 契约测试强化 |

## 五、结论建议

四项硬指标（SQLite/FTS5、schema 7 生产库读写、DPAPI 双向、数据格式兼容）**全部通过**，真实客户端（TraeWork）MCP 验证**通过**，性能基线（检索 P95 / 冷启动 / 内存 / 体积）已完整固化，另产出 2 项现网/契约缺陷的定位与修复。按迁移文档 §十九判定标准：

**建议进入阶段 2 剩余（schema v1→v7 迁移链 + 跨进程迁移互斥锁）与阶段 3（应用服务层）。**

本报告为**终版**（原两项待回填项均已完成）。唯一遗留人工项：Tauri 空壳开窗目检（偏差 2，双击 `target\release\memstack-desktop.exe` 即可，不阻塞结论）。

## 附：验证命令清单

```powershell
# C# 后端（33/33）
e:\AICoding\mcp-ai-memory\.tools\dotnet\dotnet.exe test UnifiedAiMemory.sln
# 前端（14/14）
cd memory-desktop-web; npm run test
# Rust（37/37 + fmt/clippy 全绿）
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```
