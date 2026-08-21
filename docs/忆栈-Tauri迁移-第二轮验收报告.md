# 忆栈 Tauri 迁移第二轮验收报告

> 对应计划：《忆栈-Tauri迁移-第二轮执行计划》（T1–T11）。旧《第二轮验收报告.md》为 C# 0.3.0 产品迭代的历史文档，与本报告无关。

## 一、交付结论

第二轮（阶段 2 收尾 + 阶段 3 全部）**全部完成，验收通过**，可进入阶段 4（搜索/Embedding 迁移）。

| 任务 | 状态 | 交付物 |
|---|---|---|
| T1 迁移链 + 跨进程锁 | ✅ | `named_mutex.rs` / `migrate.rs` / `migration_lock.rs` / `open_initialized` |
| T2 memory-domain 契约 | ✅ | 26 个 DTO + `checksum.rs` + `cursor.rs` + 55 错误码 |
| T3 application 基础设施 | ✅ | `db.rs` / `clock.rs` / `ids.rs` / `tokenizer.rs` |
| T4 Project/Workspace/WorkspaceIdentity | ✅ | `project_service.rs` / `workspace_service.rs` |
| T5 MemoryService + 历史版本 | ✅ | `memory_service.rs`（事务/乐观锁/FTS/版本快照） |
| T6 MemoryCandidateService | ✅ | `candidate_service.rs` |
| T7 McpAccessService | ✅ | `mcp_access.rs`（Token 三件套/DPAPI/会话状态机/轮换吊销） |
| T8 OverviewService | ✅ | `overview_service.rs` |
| T9 跨语言契约测试 | ✅ | 40 场景 C#/Rust 输出全等 |
| T10 MCP stdio 接迁移保护 | ✅ | `main.rs` 改调 `open_initialized` |
| T11 全量回归 + 本报告 | ✅ | 见 §四 |

## 二、迁移链结构等价证据

- v1→v7 迁移 SQL 自 C# `MemoryDatabase` 原文照抄（含 `randomblob()` 与 v7 段 `defer_foreign_keys`）。
- `migration_chain.rs`（5 项）：Rust 从 v1 副本逐级迁移到 v7 后，`sqlite_master` 与 C# v7 样本逐表一致；失败步骤原子回滚可恢复。
- `samples_schema7.rs`：对 C# 生成的 v7 样本库直接开库读写，结构兼容。
- WAL 只读缺陷（C# 现网 SQLite Error 10）已通过 journal_mode 探测修正并回归。

## 三、跨进程迁移锁

- 命名互斥锁 `Local\UnifiedAiMemory.Database.Migration`：快路径版本==7 不取锁；<7 时取锁（30s 上限，超时 `DATABASE_BUSY`），持锁后重读版本再迁移。
- `migration_concurrency.rs`：双连接并发初始化串行化验证；`mcp_two_process_migration.rs`：桌面 + MCP stdio 双真实进程迁移互斥验证。
- `named_mutex.rs` 单元测试：双线程串行化、超时 Timeout、Abandoned 接管。

## 四、跨语言契约测试（T9）

**结果：40 个场景全部一致**（`testdata/contract/{csharp,rust}-contract.json`，掩码规则按决策 8：`<TS>`/`<ID>`/`<SECRET>`/`<CURSOR>`、`projectCounts` 排序数组）。

覆盖：读场景（get/list/facets/revisions/projects/candidates/overview/mcp clients）、写场景（增删改、乐观锁冲突、归档恢复、候选确认/拒绝、Token 创建/轮换/吊销/删除、工作区绑定）、异常场景（错误码 + 消息逐字对齐）。

本轮修复的三个契约偏差（均为里程碑发现）：

1. **枚举序列化不一致**：C# 生产 API（`DesktopApiHost`）配置 `JsonStringEnumConverter` 输出字符串枚举，但契约工具与 `MemoryService` 内部 `JsonOptions` 漏配输出整数。修复：
   - C#：`ContractScenarios.cs` 与 `MemoryService.cs` 补 `JsonStringEnumConverter`，`memory_revision.snapshot_json` 快照改为字符串形式；
   - Rust：`memory-domain` 五个枚举（MemoryScope/MemoryStatus/McpPermission/McpAssistantType/McpClientStatus）经 `flexible_enum!` 宏实现**双形式反序列化**（名称字符串或 C# 历史整数），序列化恒为字符串——Rust 可直接读取 C# 旧版整数快照，迁移期数据双向互通。
2. **`projectCounts` 掩码漏传 key**：C# `MaskValue` 对 projectId 调用缺 `"projectId"` 参数导致 GUID 掩码分支失效，补齐后与 Rust `<ID>` 对齐。
3. **固定时钟排序歧义**：`FixedClock` 恒定时刻使 rotate 新旧 Token `created_at` 并列，「最新 Token」JOIN 排序不稳定；为 `FixedClock` 增加 `advance()`，场景在 rotate 前推进 1 分钟，与 C# 真实时钟行为一致。

## 五、全量回归（T11）

| 检查项 | 结果 |
|---|---|
| `cargo fmt --all --check` | ✅ 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 通过（修复 type-complexity / too-many-arguments 两处） |
| `cargo test --workspace` | ✅ 21 套件 127 项全过，0 失败（含契约对比、迁移链、并发锁、双进程 MCP、DPAPI 黄金数据、checksum 黄金数据） |
| `dotnet build UnifiedAiMemory.sln -c Release` | ✅ 0 警告 0 错误 |
| `dotnet test UnifiedAiMemory.sln -c Release` | ✅ 33/33 通过，无回归 |
| `cargo build --release` | ✅ Release 产物生成 |
| TraeWork 冒烟 | 用户已用当前 C# 客户端完成 `run-startup-baseline.ps1` 真实验证；本轮未动 MCP 协议，`memstack-mcp-stdio` e2e + 双进程迁移测试全绿 |

环境注记：E 盘清理导致 dotnet SDK 丢失，已重装 .NET SDK 10.0.400 至 `C:\Users\z\.dotnet`（与 Rust 工具链同策略，避免再次被清理）。

## 六、偏差与已知边界（决策对照）

- `TestClientAsync`（C# 固定端口 10212 真实 MCP HTTP 端点连接测试）：按决策 6 延后至阶段 5（stdio 子进程方案）。
- 连接池化（桌面 4 / MCP 2）：按决策 2 延后至阶段 6 性能专项；本轮 `Database{path}` 每操作开连接（WAL + busy_timeout + 重试已就绪）。
- 迁移诊断日志写入 `%LOCALAPPDATA%\UnifiedAiMemory\logs\database-migration.log`，与 C# 行为对齐、失败静默；沙箱环境下该写入被拦截但不影响功能。
- 旧《第二轮验收报告.md》文件名与本轮报告同名冲突，本轮报告以「忆栈-Tauri迁移-」前缀区分。

## 七、结论与建议

阶段 2（数据层）与阶段 3（核心业务层）迁移完成且具备跨语言等价性证据：同一场景脚本下 C# 与 Rust 输出经掩码后**逐场景字节级相等**，错误码与消息逐字对齐，历史快照数据双向互通。建议按总计划进入**阶段 4：搜索/Embedding 服务迁移**（检索管道、向量任务、RRF 融合），并在阶段 5 落地 stdio 连接测试时回收 `TestClientAsync` 偏差。
