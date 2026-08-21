# 忆栈 Tauri 迁移第三轮验收报告

> 对应计划：《忆栈-Tauri迁移-第三轮执行计划》（T1–T11，阶段 4 搜索/Embedding 全部 + 阶段 5 MCP stdio 全量迁移）。

## 一、交付结论

第三轮**全部完成，验收通过**（检查点 B 的真实客户端冒烟项待用户配合执行，见 §七），可进入阶段 6（桌面 Tauri 接线）。

| 任务 | 状态 | 交付物 |
|---|---|---|
| T1 EmbeddingService | ✅ | `embedding_service.rs`（ureq + DPAPI + 查询缓存 + 掩码） |
| T2 EmbeddingTaskWorker | ✅ | `embedding_worker.rs`（领取/指数退避/FAILED 清理/停止） |
| T3 SearchService | ✅ | `search_service.rs`（FTS5+BM25、RRF 融合、MMR、模糊回退、context 组装） |
| T4 检索契约扩展 + stdio 切换 | ✅ | 检索场景新增 ≥9 个，stdio app 切换 SearchService |
| T5 性能复跑 | ✅ | seed-20k P95：中文 32ms / 英文 67ms / 混合 25ms（< 100ms 标准，C# 基线 40ms） |
| T6 memory-mcp 契约层 | ✅ | `registry.rs` / `permissions.rs` / `dispatch.rs`（16 工具 + 权限守卫） |
| T7 stdio 16 工具分发 | ✅ | `main.rs` 全量分发 + 错误形态对齐（isError / -32602 / -32601） |
| T8 --session-id 身份 | ✅ | 密文 Token + DPAPI 解密 + hash 校验，吊销/未知会话拒绝启动 |
| T9 跨语言 MCP e2e 契约 | ✅ | `mcp_contract.rs` 3 套脚本全等（修复 C# 现网缺陷 1 处，见 §三） |
| T10 连接测试服务 | ✅ | `mcp_connection_test.rs`（stdio 握手，回收第二轮决策 6/9 偏差） |
| T11 并发验收 + 检查点 B + 本报告 | ✅ | `mcp_concurrency.rs` 5 进程并发全绿（§五）；检查点 B 待用户冒烟 |

## 二、检索与 Embedding 等价证据

- **检索管道**：FTS5 关键词召回（BM25 权重）+ 语义召回 + 加权 RRF 融合 + 模糊回退（无命中时降级匹配）+ titleExact 断言 + MMR 多样性截断 + context 组装（字符预算约束）——与 C# `SearchService` 场景逐一对齐，新增检索/上下文场景掩码后全等。
- **EmbeddingService**：ureq 2（rustls + native-certs）HTTP 客户端、30s 超时逐字对齐、DPAPI 密钥加解密、查询向量缓存、维度校验、输入掩码（日志不泄露正文）；契约场景固定 keyword 模式，不依赖真实 Embedding API，语义路径以注入向量单测覆盖。
- **EmbeddingTaskWorker**：任务领取、指数退避、FAILED 终态清理、stop 信号无残留——全部单测覆盖。

## 三、本轮修复的现网缺陷（跨语言契约发现）

**C# `MemoryCandidateService.ListAsync` SQL 拼接语法错误**（现网缺陷，REST 与 MCP 双路径均损坏）：

- 现象：跨语言契约第 12 帧（`memory_candidate_list`）C# 侧返回 `isError`（SDK 通用摘要 "An error occurred invoking 'memory_candidate_list'."），且 C# stdio 宿主 `ClearProviders()` 吞掉全部异常日志，表象与根因完全脱节。
- 定位过程：MCP 层不可见 → C# 单测直调 `ListAsync` 复现 `SQLite Error 1: 'near "c": syntax error'` → SQL 变体二分（`$`/`@` 前缀、无参子句均合法）→ 反射读取 DLL 内常量原始值，确认拼接结果。
- 根因：C# raw string literal（`"""..."""`）**闭引号前的换行被剥离**，`SelectCandidateSql` 常量末尾无换行，与 WHERE 子句拼接后变成 `...p.id=c.project_idWHERE c.status=...`。同仓库 `SearchService` 的同类拼接处显式补了 `"\n"`，唯独此处遗漏。
- 修复：`SelectCandidateSql + "\n" + """WHERE..."""`（对齐 `SearchService` 写法）；新增 C# 回归测试 `CandidateList_CompilesSqlOnSampleDatabase`（34/34 全绿，含此前 33 项无回归）；重编 Release exe 后两侧契约帧全等。
- Rust 侧对应实现使用 `format!("{SELECT_CANDIDATE_SQL} WHERE ...")` 显式空格，无此问题。

## 四、MCP stdio 全量 16 工具（阶段 5）

- **tools/list**：以 `contracts/mcp-tools-list.json` 快照为唯一权威，恰 16 工具，与 C# mcp-child 输出 Value 相等（键序无关）。
- **tools/call 形态对齐**（实测 C# mcp-child 为规范）：
  - 业务/参数错误 → `result.isError=true` + 固定英文摘要（会话存活）；
  - 未知工具 → JSON-RPC `-32602`；未知方法 → `-32601`；
  - `structuredContent` 空值字段整体省略（null 属性不输出）；void 工具 → `content: []`。
- **身份双轨**：`--session-id <guid>` 主路径（密文 Token + DPAPI + hash 校验，「最新未吊销、created_at 倒序」与 C# `GetClientSecretAsync` 同规则）；`UNIFIED_AI_MEMORY_TOKEN` 兼容保留（阶段 8 移除）。吊销/未知会话启动即失败（非 0 退出）。
- **连接测试服务（T10）**：spawn exe → initialize（协议版本校验）+ tools/list（16 工具校验）→ 中文报告 + 耗时；默认 10s 超时杀进程；无任何 10212 HTTP 依赖（回收第二轮决策 6 偏差）。假 exe 成功/超时/清单不符三路径单测 + 真实 exe 端到端握手全绿。

## 五、并发验收（T11-1）

`mcp_concurrency.rs`：**5 个 MCP 进程同库并发**（4 全量 Token + 1 项目绑定 Token）执行「读 + 写 + 候选 + 项目」混合脚本：

- 无 SQLITE_BUSY 泄漏：全部写操作成功（busy_timeout + 排队消化竞争；耗尽会表现为 isError）；
- 无跨项目越权：项目 Token 写个人范围被拒（isError）、绑定项目内写入成功、项目列表仅见绑定项目；
- 终态一致：memory +5、candidate +4 且 FTS 与记忆行同步，与脚本成功数严格相等；
- 5 进程全部退出码 0、响应帧数与请求一致。

## 六、全量回归（T11-3）

| 检查项 | 结果 |
|---|---|
| `cargo fmt --all --check` | ✅ 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 通过（修复 doc_lazy_continuation ×3、len_zero ×1） |
| `cargo test --workspace` | ✅ 24 套件 177 项全过，0 失败（含契约 3 脚本、并发验收、真实 exe 握手、seed-20k 性能） |
| `dotnet build UnifiedAiMemory.sln -c Release` | ✅ 0 警告 0 错误 |
| `dotnet test UnifiedAiMemory.Api.Tests -c Release` | ✅ 34/34 通过（新增候选列表回归测试 1 项，此前 33 项无回归） |
| `cargo build --release -p memstack-mcp-stdio` | ✅ Release 产物生成 |
| 跨语言 MCP 契约 | ✅ 主脚本 21 帧 + 只读越权 3 帧 + 项目范围 5 帧，两侧掩码后逐帧全等 |

## 七、检查点 B 与待办

- **检查点 B（真实客户端冒烟，需用户配合）**：Codex 与第二客户端（TraeWork）均以 `--session-id` 配置自动启动 `MemStack-MCP.exe`（`rust/target/release/memstack-mcp-stdio.exe` 产物路径或安装后路径），确认「已连接」状态来自真实 initialize（`mcp_client_session.last_seen_at` 回填）。**在两个真实客户端稳定调用通过前，检查点 B 判定为待验证，不阻塞代码验收。**
- 第二轮遗留的 `TestClientAsync`（10212 HTTP 连接测试）偏差已由 stdio 握手方案回收（决策 9），10212 依赖全部清除。
- env Token 兼容路径按计划在阶段 8 依赖审计时移除。

## 八、结论与建议

阶段 4（搜索/Embedding）与阶段 5（MCP stdio 全量）迁移完成：检索管道场景级等价、P95 显著优于标准线、16 工具与错误形态与 C# mcp-child 逐帧全等、5 进程并发无锁泄漏、身份双轨与权限守卫闭环。过程中还发现并修复了一处 C# 现网缺陷（候选列表 SQL 拼接），两侧测试数同步增长。

建议：用户完成检查点 B 真实客户端冒烟后，按总计划进入**阶段 6：桌面 Tauri 接线**（Tauri Command、连接池化、`test_mcp_client` 桌面命令接线、EmbeddingTaskWorker 桌面消费）。
