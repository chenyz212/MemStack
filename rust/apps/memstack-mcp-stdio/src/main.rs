//! MemStack-MCP：无窗口的 MCP stdio 服务。
//!
//! 进程规则（对应迁移文档 §8.1）：
//! - 不创建窗口、托盘和 HTTP 监听。
//! - stdout 只写 MCP 协议消息（JSON-RPC over NDJSON）。
//! - 日志写入 stderr 与 `%LOCALAPPDATA%\MemStack\logs\mcp-stdio.log`。
//! - stdin EOF 后正常退出（退出码 0）；鉴权失败以非 0 退出码退出。
//! - 进程回收三层防线：① 客户端关闭管道 → stdin EOF 秒级退出；
//!   ② 客户端进程死亡（EOF 未触发的场景）→ 祖先监控轮询发现后退出；
//!   ③ 祖先链不可解析时退化为空闲超时兜底（默认 24h）。
//!
//! 工具分发与权限守卫由 `memory-mcp` 契约层承载（契约权威：contracts/mcp-tools-list.json）；
//! 错误响应形态（沿自 C# 时代实测快照，现为稳定行为契约）：
//! - 业务/参数错误 → result.isError=true + content 文本「[错误码] 中文消息
//!   （调用工具：xxx）」，不携带 structuredContent（错误形态不符合 outputSchema，
//!   MCP 客户端校验会吞掉真实原因），会话存活。
//! - 未知工具 → JSON-RPC -32602；未知方法 → -32601。
//!
//! 身份双轨（T8）：主路径 `MemStack-MCP --session-id <guid>`（密文 Token + DPAPI 解密
//! 并校验 hash，与 C# `GetClientSecretAsync` 同规则：最新未吊销、created_at 倒序）；
//! 兼容路径环境变量 `MEMSTACK_TOKEN`（阶段 8 依赖审计时移除）。

mod auth;

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use memory_application::candidate_service::MemoryCandidateService;
use memory_application::embedding_service::{EmbeddingService, UreqEmbeddingClient};
use memory_application::memory_service::MemoryService;
use memory_application::project_service::ProjectService;
use memory_application::search_service::{EmbeddingQueryVectors, SearchService};
use memory_application::workspace_service::WorkspaceService;
use memory_application::{Clock, Database, GuidGenerator, SystemClock, format_storage_time};
use memory_domain::McpCallerContext;
use memory_mcp::dispatch::Services;
use serde_json::{Value, json};

/// 协议版本与当前桌面端服务器信息。
const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "memstack";
const SERVER_TITLE: &str = "MemStack";
const SERVER_VERSION: &str = "0.4.1";
/// stdin 空闲超时默认值（秒）：24 小时，仅退化模式（祖先链不可解析）兜底。
///
/// 历史教训：空闲超时曾被当作主要回收手段（先 5 分钟后 30 分钟），但部分 AI 客户端
/// 空闲时既不发帧也不关管道——任何有限超时都会误杀安静的真连接，且客户端不会自动
/// 重启死掉的 MCP 进程，误杀 = 本会话 MCP 报废。正确判活信号是客户端进程本身
/// （见 [`resolve_ancestor_watch`]）；空闲超时仅在祖先链不可解析的罕见场景生效，
/// 宁晚勿早——晚杀只是一个轻量进程多存活一会儿，误杀是会话报废。
const DEFAULT_STDIN_IDLE_TIMEOUT_SECS: u64 = 86400;

/// 祖先进程存活轮询间隔（默认 30 秒）。
const DEFAULT_ANCESTOR_POLL_INTERVAL: Duration = Duration::from_secs(30);

const SERVER_INSTRUCTIONS: &str = concat!(
    "记忆保存执行规范（严格遵守）：",
    "1. 会话内首次保存记忆前调用 project_resolve，传入用户当前工作空间根目录（完整路径或目录名，从对话上下文获知）；纯对话等无工作空间场景不传参数；",
    "2. 返回 MAPPED：当前处于项目空间，此后所有记忆必须以 scope=Project 并携带该 projectId 保存，禁止存入个人记忆；",
    "3. 返回 PROJECT_NAME_REQUIRED：当前工作空间尚未绑定项目——用一句话向用户询问该工作空间对应的中文项目名称（禁止编造），得到答复后调用 project_create（name=用户答复的中文名称，workspaceIdentifier=本响应返回的 workspaceIdentifier），再以 scope=Project 携带新 projectId 保存；",
    "4. 返回 UNBOUND：无工作空间上下文（纯对话），一律以 scope=Personal 保存个人记忆，禁止向用户询问项目名；",
    "5. 不要猜测或编造工作空间标识：只能使用对话上下文中明确的工作空间信息，或 project_resolve 的返回结果；",
    "6. 【必看参数取值】memoryType 和 importance 必须按以下枚举填写，禁止编造，填写错误会直接失败：",
    "   - memoryType 仅支持（9 选 1，大小写不敏感）：",
    "     NOTE(笔记：通用记录) / PREFERENCE(偏好：用户选择和喜好) / DECISION(决策：做出的重要决定) / ",
    "     SOLUTION(方案：问题的解决办法) / FACT(事实：客观事实) / CONVENTION(约定：编码/流程/规范约定) / ",
    "     TASK(任务：待办/已办任务) / CONTEXT(上下文：项目/会话背景信息) / OTHER(其他：无法归类的内容)；",
    "   - importance 仅支持整数 1~5，分别对应：1(可遗忘) / 2(普通) / 3(重要，默认值) / 4(非常重要) / 5(核心，长期记忆)。",
    "7. 工具调用若返回 isError=true，content 中会带有 [错误码] 中文消息格式的具体原因，请根据错误消息修正参数后重试，不要反复使用相同错误参数调用。"
);

fn main() {
    if let Err(failure) = run() {
        // 致命错误：stderr 摘要（不含敏感信息）+ 非 0 退出码。
        eprintln!("[MemStack-MCP] 启动失败：{failure}");
        append_log(&format!("startup failure: {failure}"));
        std::process::exit(1);
    }
    eprintln!("[MemStack-MCP] stdin EOF，正常退出");
}

fn run() -> Result<(), String> {
    // 品牌升级目录迁移（必须在任何 local_app_data_dir() 调用前执行）。
    // 0.4.0 起 UnifiedAiMemory → MemStack；旧目录存在则整体重命名迁移。
    memory_platform::migrate_legacy_dir_if_needed().map_err(|error| format!("{error}（请退出旧版本后重启）"))?;
    // §16 启动时日志轮换：失败静默（诊断辅助不阻断启动）。
    // 测试子进程不动生产日志，也不触发轮换。
    if !test_db_scenario() {
        memory_platform::log_rotation::rotate_logs();
    }
    let database_path = resolve_database_path()?;
    append_log(&format!("database: {}", database_path.display()));
    // §17.1 首次运行备份：与桌面进程共用同一安全网（marker 幂等；显式
    // MEMSTACK_DB_PATH 的测试场景跳过）。失败即退出，不写 marker 待重试。
    if !test_db_scenario() {
        memory_storage::ensure_first_run_backup(&database_path)
            .map_err(|error| format!("创建首次运行备份失败：{}", error.message))?;
    }
    // open_initialized：与桌面进程共用同一入口，先取迁移锁再按需升级结构（§6.3）。
    let connection = memory_storage::open_initialized(&database_path)
        .map_err(|error| format!("打开数据库失败：{}", error.message))?;
    memory_storage::verify_fts5(&connection).map_err(|error| format!("FTS5 校验失败：{}", error.message))?;

    let plain_token = load_plain_token(&connection)?;
    let caller = auth::authenticate(&connection, &plain_token)?;
    append_log(&format!(
        "authenticated caller='{}' permission={:?}",
        caller.display_name, caller.permission
    ));
    touch_token(&connection, &caller);

    // 装配 memory-application 服务并交给 memory-mcp 契约层分发；
    // 查询向量经 EmbeddingService（配置未启用时自动回退关键词模式）。
    let database = Database::new(&database_path);
    let clock = Arc::new(SystemClock);
    let ids = Arc::new(GuidGenerator);
    let memories = Arc::new(MemoryService::new(database.clone(), clock.clone(), ids.clone()));
    let projects = Arc::new(ProjectService::new(database.clone(), clock.clone(), ids.clone()));
    let workspaces = Arc::new(WorkspaceService::new(
        database.clone(),
        projects.clone(),
        memories.clone(),
        clock.clone(),
    ));
    let candidates = Arc::new(MemoryCandidateService::new(
        database.clone(),
        memories.clone(),
        clock.clone(),
        ids.clone(),
    ));
    let embedding = Arc::new(EmbeddingService::new(
        database.clone(),
        clock.clone(),
        ids.clone(),
        Arc::new(UreqEmbeddingClient),
    ));
    let search = Arc::new(SearchService::new(
        database.clone(),
        Arc::new(EmbeddingQueryVectors::new(embedding)),
    ));
    let documents = Arc::new(
        memory_application::project_document_service::ProjectDocumentService::new(
            database.clone(),
            clock.clone(),
            ids.clone(),
        ),
    );
    let conclusion_cards = Arc::new(memory_application::conclusion_card_service::ConclusionCardService::new(
        database.clone(),
        candidates.clone(),
    ));
    // 最近记忆活动记录器：成功的读取与写入操作均落库。
    let activity = Arc::new(memory_application::mcp_access::MemoryActivityRecorder::new(
        database,
        clock.clone(),
    ));
    let services = Services {
        memories,
        candidates,
        projects,
        workspaces,
        search,
        documents,
        conclusion_cards,
        activity: Some(activity),
        // AI 客户端以自身项目目录为 cwd 拉起本进程；部分客户端（WorkBuddy/TraeWork）
        // 的 cwd 是连接实例目录（如 custom-mcp_<server>-<hash>），非用户工作空间——
        // dispatch 层仅采纳形如真实路径的 hint（is_path_like_hint），实例目录自动忽略。
        workspace_hint: std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(str::to_string)),
    };
    append_log(&format!(
        "workspace_hint={:?} path_like={}",
        services.workspace_hint,
        services
            .workspace_hint
            .as_deref()
            .map(memory_mcp::dispatch::is_path_like_hint)
            .unwrap_or(false)
    ));

    let idle_timeout = resolve_idle_timeout();
    let ancestor_watch = resolve_ancestor_watch();
    append_log(&format!(
        "idle_timeout={}s degraded_only={}",
        idle_timeout.map_or(0, |duration| duration.as_secs()),
        ancestor_watch.is_none()
    ));

    serve_stdio(&caller, &services, idle_timeout, ancestor_watch)
}

/// 客户端祖先监控句柄（被监控进程 + 轮询间隔）。
struct AncestorWatch {
    process: memory_platform::process_tree::AncestorProcess,
    poll_interval: Duration,
}

/// 解析客户端祖先监控：本进程是被谁拉起的。
///
/// - 测试缝 `MEMSTACK_MCP_FAKE_ANCESTOR_PID`：直接监控指定 PID（e2e 验证
///   「杀牺牲进程 → MCP 随之退出」的正向回归），进程名从启动快照解析（查无则仅按 PID）；
/// - 生产路径：Toolhelp32 快照沿 PPID 链向上，跳过 cmd/powershell 等壳，锁定最近的
///   非壳祖先进程（即客户端本体）。
///
/// 返回 `None`（退化模式）：快照失败 / 父链断裂 / 到达系统常驻进程——此时由
/// 空闲超时兜底回收。
fn resolve_ancestor_watch() -> Option<AncestorWatch> {
    use memory_platform::process_tree as tree;

    let poll_interval = resolve_ancestor_poll_interval();
    if let Ok(raw) = std::env::var("MEMSTACK_MCP_FAKE_ANCESTOR_PID")
        && let Ok(pid) = raw.trim().parse::<u32>()
    {
        let name = tree::snapshot_processes()
            .ok()
            .and_then(|table| table.into_iter().find(|entry| entry.pid == pid))
            .map(|entry| entry.name);
        append_log(&format!("ancestor_watch source=fake pid={pid} name={name:?}"));
        return Some(AncestorWatch {
            process: tree::AncestorProcess { pid, name },
            poll_interval,
        });
    }
    let Ok(table) = tree::snapshot_processes() else {
        append_log("ancestor_watch source=none reason=snapshot_failed → degraded（空闲超时兜底）");
        return None;
    };
    match tree::resolve_client_ancestor(&table, std::process::id()) {
        Some(process) => {
            append_log(&format!(
                "ancestor_watch source=ppid pid={} name={:?}",
                process.pid, process.name
            ));
            Some(AncestorWatch { process, poll_interval })
        }
        None => {
            append_log("ancestor_watch source=none reason=chain_unresolvable → degraded（空闲超时兜底）");
            None
        }
    }
}

/// 解析祖先轮询间隔：`MEMSTACK_MCP_ANCESTOR_POLL_MS`（正整数毫秒，测试可调快），
/// 未设置或非法时用默认 30s。
fn resolve_ancestor_poll_interval() -> Duration {
    let Ok(raw) = std::env::var("MEMSTACK_MCP_ANCESTOR_POLL_MS") else {
        return DEFAULT_ANCESTOR_POLL_INTERVAL;
    };
    match raw.trim().parse::<u64>() {
        Ok(ms) if ms > 0 => Duration::from_millis(ms),
        _ => DEFAULT_ANCESTOR_POLL_INTERVAL,
    }
}

/// 解析 stdin 空闲超时：`MEMSTACK_MCP_IDLE_TIMEOUT_SECS` 环境变量优先
/// （正整数秒；0 = 禁用超时），未设置或值非法时用默认值 24 小时。
/// 仅退化模式（祖先监控不可用）生效；正常模式下判活交给进程存亡，此值无意义。
fn resolve_idle_timeout() -> Option<Duration> {
    let Ok(raw) = std::env::var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS") else {
        return Some(Duration::from_secs(DEFAULT_STDIN_IDLE_TIMEOUT_SECS));
    };
    match raw.trim().parse::<u64>() {
        Ok(0) => {
            append_log("MEMSTACK_MCP_IDLE_TIMEOUT_SECS=0，空闲超时已禁用");
            None
        }
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => {
            append_log(&format!(
                "MEMSTACK_MCP_IDLE_TIMEOUT_SECS 值无效（{raw:?}），使用默认值 {DEFAULT_STDIN_IDLE_TIMEOUT_SECS}s"
            ));
            Some(Duration::from_secs(DEFAULT_STDIN_IDLE_TIMEOUT_SECS))
        }
    }
}

/// 身份双轨加载（T8）：
/// 1. 主路径：命令行 `--session-id <guid>` → 按「最新未吊销、created_at 倒序」取
///    `mcp_token.token_ciphertext`（与 C# `GetClientSecretAsync` 同 SQL），DPAPI 解密得明文。
///    会话不存在 / 全部吊销 / DPAPI 失败均致命退出（客户端新初始化失败）。
/// 2. 兼容路径：环境变量 `MEMSTACK_TOKEN`（阶段 8 移除）。
fn load_plain_token(connection: &rusqlite::Connection) -> Result<String, String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if let Some(position) = arguments.iter().position(|argument| argument == "--session-id")
        && let Some(session_id) = arguments.get(position + 1)
    {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err("--session-id 参数不能为空".to_string());
        }
        let ciphertext: Option<String> = connection
            .query_row(
                "SELECT token_ciphertext FROM mcp_token \
                 WHERE session_id=?1 AND revoked_at IS NULL \
                 ORDER BY created_at DESC LIMIT 1;",
                [session_id],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(|error| format!("按会话读取 Token 失败：{error}"))?;
        let Some(ciphertext) = ciphertext.filter(|text| !text.is_empty()) else {
            return Err("会话不存在、Token 已吊销或为旧版 Token，请重新生成".to_string());
        };
        return memory_platform::dpapi::unprotect(&ciphertext)
            .map_err(|_| "当前 Windows 用户无法解密该会话 Token，请重新生成".to_string());
    }
    std::env::var("MEMSTACK_TOKEN").map_err(|_| "缺少 MEMSTACK_TOKEN 环境变量".to_string())
}

/// 启动时记录连接活动（与 C# stdio 每进程一次 AuthenticateAsync → TouchAsync 对齐：
/// last_used_at + 会话 call_count+1/last_seen_at）。
fn touch_token(connection: &rusqlite::Connection, caller: &McpCallerContext) {
    let now_text = format_storage_time(SystemClock.now_utc());
    let _ = connection.execute(
        "UPDATE mcp_token SET last_used_at=?1 WHERE id=?2;",
        rusqlite::params![now_text, caller.token_id],
    );
    let session_id: Option<String> = connection
        .query_row(
            "SELECT session_id FROM mcp_token WHERE id=?1;",
            [&caller.token_id],
            |row| row.get(0),
        )
        .unwrap_or(None);
    if let Some(session_id) = session_id {
        let _ = connection.execute(
            "UPDATE mcp_client_session \
             SET last_seen_at=?1, call_count=call_count+1, updated_at=?1 WHERE id=?2;",
            rusqlite::params![now_text, session_id],
        );
    }
}

/// 解析数据库路径：优先 `MEMSTACK_DB_PATH`（测试/显式指定），否则走生产数据目录规则。
fn resolve_database_path() -> Result<std::path::PathBuf, String> {
    if let Ok(explicit) = std::env::var("MEMSTACK_DB_PATH")
        && !explicit.trim().is_empty()
    {
        return Ok(std::path::PathBuf::from(explicit));
    }
    let data_dir = memory_platform::data_dir().map_err(|error| format!("解析数据目录失败：{error}"))?;
    memory_storage::resolve_desktop_database_path(&data_dir)
        .map_err(|error| format!("解析数据库路径失败：{}", error.message))
}

/// JSON-RPC 帧循环：逐行读取 stdin，响应写 stdout，日志走 stderr/文件。
///
/// 进程回收（三层防线，见模块文档）：
/// - 祖先监控生效（`ancestor` 为 `Some`）：每 `poll_interval` 醒来查一次客户端进程
///   是否存活，死亡即退出；此时空闲超时完全禁用——安静的真连接永不被时间误杀。
/// - 退化模式（`ancestor` 为 `None`）：`idle_timeout` 兜底回收（`None` 表示禁用），
///   仅在祖先链不可解析的罕见场景下到达。
fn serve_stdio(
    caller: &McpCallerContext,
    services: &Services,
    idle_timeout: Option<Duration>,
    ancestor: Option<AncestorWatch>,
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();

    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(text) => {
                    if tx.send(Ok(text)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(format!("读取 stdin 失败：{error}")));
                    break;
                }
            }
        }
    });

    let mut stdout = std::io::stdout();
    // 退化模式的空闲截止时刻；祖先监控生效时恒为 None（不参与退出决策）。
    let mut idle_deadline = if ancestor.is_some() {
        None
    } else {
        idle_timeout.map(|timeout| std::time::Instant::now() + timeout)
    };

    loop {
        // 等待粒度：祖先监控 → 轮询间隔；退化模式 → 距空闲截止的剩余时间（无截止则阻塞）。
        let received = match &ancestor {
            Some(watch) => rx.recv_timeout(watch.poll_interval),
            None => match idle_deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        Err(mpsc::RecvTimeoutError::Timeout)
                    } else {
                        rx.recv_timeout(remaining)
                    }
                }
                None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            },
        };
        match received {
            Ok(Ok(line)) => {
                // 任何帧（含空行、通知、ping）重置退化模式的空闲计时。
                if ancestor.is_none()
                    && let Some(timeout) = idle_timeout
                {
                    idle_deadline = Some(std::time::Instant::now() + timeout);
                }
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    append_log(&format!("dropped non-JSON frame: {} bytes", line.len()));
                    continue;
                };
                let Some(id) = request.get("id").cloned() else {
                    append_log(&format!("notification: {}", request["method"].as_str().unwrap_or("")));
                    continue;
                };
                let method = request["method"].as_str().unwrap_or("").to_string();
                let result = match method.as_str() {
                    "initialize" => initialize_result(),
                    // MCP 规范：ping 须回空对象；任何帧（含 ping）都会重置退化模式的
                    // 空闲计时器，发心跳的客户端因此不会被误杀。
                    "ping" => json!({}),
                    "tools/list" => memory_mcp::registry::tools_list_json(),
                    "tools/call" => match tools_call(caller, services, &request["params"]) {
                        ToolOutcome::Success(value) => value,
                        ToolOutcome::Failed(message) => {
                            write_response(&mut stdout, &error_response(id, -32602, &message))?;
                            continue;
                        }
                    },
                    other => {
                        write_response(
                            &mut stdout,
                            &error_response(id, -32601, &format!("Method '{other}' is not available.")),
                        )?;
                        continue;
                    }
                };
                let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                write_response(&mut stdout, &response)?;
            }
            Ok(Err(error)) => {
                append_log(&error);
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 祖先监控：查客户端进程存活；快照失败按存活处理（瞬时故障不误杀）。
                if let Some(watch) = &ancestor {
                    match memory_platform::process_tree::snapshot_processes() {
                        Ok(table) if watch.process.is_alive_in(&table) => continue,
                        Ok(_) => {
                            append_log(&format!(
                                "ancestor_exit pid={} name={:?}（客户端进程已退出，回收本连接）",
                                watch.process.pid, watch.process.name
                            ));
                            eprintln!("[MemStack-MCP] 客户端祖先进程已退出，准备退出");
                            return Ok(());
                        }
                        Err(error) => {
                            append_log(&format!("ancestor_check_failed: {}", error.message));
                            continue;
                        }
                    }
                }
                // 退化模式：空闲超时兜底。
                let secs = idle_timeout.map_or(0, |duration| duration.as_secs());
                append_log(&format!("stdin 空闲超时（{secs}s 无消息），AI 客户端可能已断开"));
                eprintln!("[MemStack-MCP] stdin 空闲超时（{secs}s），准备退出");
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Ok(());
            }
        }
    }
}

/// 工具调用三态：成功载荷 / 协议级失败（未知工具）。
/// 业务与参数错误不出现在这里 —— 它们按 C# 实测形态包装为 isError 结果返回。
enum ToolOutcome {
    Success(Value),
    Failed(String),
}

fn tools_call(caller: &McpCallerContext, services: &Services, params: &Value) -> ToolOutcome {
    let name = params["name"].as_str().unwrap_or("");
    let started = std::time::Instant::now();
    // 与 C# SDK 实测一致：Unknown tool: '<name>'。
    let outcome = if !memory_mcp::registry::is_known_tool(name) {
        ToolOutcome::Failed(format!("Unknown tool: '{name}'"))
    } else {
        match memory_mcp::dispatch::dispatch(name, &params["arguments"], caller, services) {
            Ok(value) => ToolOutcome::Success(shape_tool_success(value)),
            Err(error) => ToolOutcome::Success(shape_tool_error(name, error)),
        }
    };
    // 观测埋点：服务端处理耗时落日志（ok/error/unknown_tool 三态 +
    // args_bytes 参数体积），把「慢调用」与「大参数」关联起来，可观测可归因。
    append_log(&format!(
        "tool_call name={name} outcome={} elapsed_ms={} args_bytes={}",
        tool_outcome_label(&outcome),
        started.elapsed().as_millis(),
        serde_json::to_string(&params["arguments"]).map_or(0, |text| text.len()),
    ));
    outcome
}

/// 观测日志的结果标签：成功 / 业务错误（isError 结果，会话存活）/ 未知工具。
fn tool_outcome_label(outcome: &ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Success(value) if value.get("isError").is_some() => "error",
        ToolOutcome::Success(_) => "ok",
        ToolOutcome::Failed(_) => "unknown_tool",
    }
}

/// 成功形态（实测 C# mcp-child）：
/// - 空值字段整体省略（MCP SDK 序列化语义：null 属性不输出）。
/// - void 工具 → `{"content":[]}`；
/// - 列表工具（structuredContent={"result":[...]}）→ text 为裸数组序列化；
/// - 单对象工具 → text 为对象序列化。
fn shape_tool_success(value: Value) -> Value {
    if value.is_null() {
        return json!({ "content": [] });
    }
    let value = strip_nulls(value);
    let text_value = match &value {
        Value::Object(map) if map.len() == 1 && map.contains_key("result") => map["result"].clone(),
        _ => value.clone(),
    };
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string(&text_value).unwrap_or_default() }],
        "structuredContent": value,
    })
}

/// 递归移除值为 null 的对象属性（对齐 C# MCP SDK structuredContent 序列化：
/// projectId/projectName/archivedAt 等可空字段为 null 时不输出键）。
fn strip_nulls(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(strip_nulls).collect()),
        Value::Object(map) => {
            let mut output = serde_json::Map::new();
            for (key, item) in map {
                if item.is_null() {
                    continue;
                }
                output.insert(key, strip_nulls(item));
            }
            Value::Object(output)
        }
        other => other,
    }
}

/// 业务/参数错误形态：isError 结果 + content 文本「[错误码] 中文消息（调用工具：xxx）」。
///
/// 不携带 structuredContent：outputSchema 描述的是成功载荷（id/scope/title…），
/// 错误详情一旦放进 structuredContent，MCP 客户端（TraeWork/WorkBuddy）会按
/// outputSchema 校验并整体拒绝，真实错误消息到不了 LLM——实测曾把
/// MCP_PROJECT_SCOPE_DENIED 伪装成「Structured content does not match the
/// tool's output schema」，排查成本极高。isError=true + 纯 content 文本
/// 是 MCP 规范允许且 C# 时代验证过的兼容形态。
fn shape_tool_error(name: &str, error: memory_domain::BusinessError) -> Value {
    let code = error.code.as_str();
    let message = &error.message;
    json!({
        "content": [{
            "type": "text",
            "text": format!("[{code}] {message}（调用工具：{name}）"),
        }],
        "isError": true,
    })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "logging": {}, "tools": { "listChanged": true } },
        "serverInfo": { "name": SERVER_NAME, "title": SERVER_TITLE, "version": SERVER_VERSION },
        "instructions": SERVER_INSTRUCTIONS,
    })
}

fn write_response(stdout: &mut std::io::Stdout, response: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *stdout, response).map_err(|error| format!("序列化响应失败：{error}"))?;
    stdout
        .write_all(b"\n")
        .and_then(|_| stdout.flush())
        .map_err(|error| format!("写入 stdout 失败：{error}"))
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// 测试场景判定：显式 `MEMSTACK_DB_PATH`（测试/诊断数据库）时，本进程
/// 不触碰生产日志（不写入、不轮换）——cargo test 会并发拉起大量 MCP 子进程，
/// 逐条写入会污染真实运行日志。
fn test_db_scenario() -> bool {
    std::env::var("MEMSTACK_DB_PATH").is_ok_and(|value| !value.trim().is_empty())
}

/// 解析 mcp-stdio.log 路径：
/// - `MEMSTACK_MCP_LOG_FILE`（测试钩子）：重定向到指定文件；
/// - 单元测试进程 / 显式 `MEMSTACK_DB_PATH` 的子进程：不写文件；
/// - 生产：`%LOCALAPPDATA%\MemStack\logs\mcp-stdio.log`。
fn log_file_path() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("MEMSTACK_MCP_LOG_FILE")
        && !explicit.trim().is_empty()
    {
        return Some(std::path::PathBuf::from(explicit));
    }
    if cfg!(test) || test_db_scenario() {
        return None;
    }
    memory_platform::logs_dir().ok().map(|dir| dir.join("mcp-stdio.log"))
}

/// 追加日志到 mcp-stdio.log（路径由 [`log_file_path`] 决定）；失败时静默忽略。
fn append_log(message: &str) {
    use std::io::Write as _;

    let Some(path) = log_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let _ = writeln!(file, "[{timestamp}] [pid={}] {message}", std::process::id());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量类测试的串行锁（避免并行 env 竞态）。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn idle_timeout_defaults_to_24_hours_degraded_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::remove_var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS") };
        assert_eq!(
            resolve_idle_timeout(),
            Some(Duration::from_secs(DEFAULT_STDIN_IDLE_TIMEOUT_SECS))
        );
        assert_eq!(DEFAULT_STDIN_IDLE_TIMEOUT_SECS, 86400);
    }

    #[test]
    fn idle_timeout_env_overrides_and_zero_disables() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::set_var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS", "60") };
        assert_eq!(resolve_idle_timeout(), Some(Duration::from_secs(60)));
        unsafe { std::env::set_var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS", "0") };
        assert_eq!(resolve_idle_timeout(), None, "0 必须禁用超时");
        unsafe { std::env::set_var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS", " 120 ") };
        assert_eq!(resolve_idle_timeout(), Some(Duration::from_secs(120)), "允许首尾空白");
        unsafe { std::env::set_var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS", "abc") };
        assert_eq!(
            resolve_idle_timeout(),
            Some(Duration::from_secs(DEFAULT_STDIN_IDLE_TIMEOUT_SECS)),
            "非法值回退默认"
        );
        unsafe { std::env::set_var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS", "") };
        assert_eq!(
            resolve_idle_timeout(),
            Some(Duration::from_secs(DEFAULT_STDIN_IDLE_TIMEOUT_SECS)),
            "空串视为未设置"
        );
        // SAFETY：同上。
        unsafe { std::env::remove_var("MEMSTACK_MCP_IDLE_TIMEOUT_SECS") };
    }

    #[test]
    fn log_file_path_suppresses_tests_and_honors_redirect() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::remove_var("MEMSTACK_DB_PATH") };
        unsafe { std::env::remove_var("MEMSTACK_MCP_LOG_FILE") };
        // 单元测试进程（cfg!(test)）默认不写日志文件。
        assert!(log_file_path().is_none(), "测试进程默认抑制日志文件");
        // MEMSTACK_MCP_LOG_FILE 重定向优先于抑制规则（观测埋点 e2e 依赖它）。
        unsafe { std::env::set_var("MEMSTACK_MCP_LOG_FILE", r"C:\tmp\observe.log") };
        assert_eq!(
            log_file_path().as_deref(),
            Some(std::path::Path::new(r"C:\tmp\observe.log"))
        );
        unsafe { std::env::remove_var("MEMSTACK_MCP_LOG_FILE") };
    }

    #[test]
    fn ancestor_watch_prefers_fake_pid_and_custom_poll_interval() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::set_var("MEMSTACK_MCP_FAKE_ANCESTOR_PID", std::process::id().to_string()) };
        unsafe { std::env::set_var("MEMSTACK_MCP_ANCESTOR_POLL_MS", "1500") };
        let watch = resolve_ancestor_watch().expect("fake 祖先监控应生效");
        assert_eq!(watch.process.pid, std::process::id());
        // 进程名从快照解析（cargo 测试二进制名把连字符替换为下划线并带 hash 后缀，
        // 归一化后只断言前缀）。
        assert!(
            watch
                .process
                .name
                .as_deref()
                .is_some_and(|name| name.to_ascii_lowercase().replace('_', "-").starts_with("memstack-mcp")),
            "快照应解析出本测试进程名：{:?}",
            watch.process.name
        );
        assert_eq!(watch.poll_interval, Duration::from_millis(1500));
        unsafe { std::env::remove_var("MEMSTACK_MCP_FAKE_ANCESTOR_PID") };
        unsafe { std::env::remove_var("MEMSTACK_MCP_ANCESTOR_POLL_MS") };
    }

    #[test]
    fn ancestor_poll_interval_defaults_and_rejects_invalid() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::remove_var("MEMSTACK_MCP_ANCESTOR_POLL_MS") };
        assert_eq!(resolve_ancestor_poll_interval(), DEFAULT_ANCESTOR_POLL_INTERVAL);
        assert_eq!(DEFAULT_ANCESTOR_POLL_INTERVAL, Duration::from_secs(30));
        unsafe { std::env::set_var("MEMSTACK_MCP_ANCESTOR_POLL_MS", "0") };
        assert_eq!(
            resolve_ancestor_poll_interval(),
            DEFAULT_ANCESTOR_POLL_INTERVAL,
            "0 回退默认"
        );
        unsafe { std::env::set_var("MEMSTACK_MCP_ANCESTOR_POLL_MS", "abc") };
        assert_eq!(
            resolve_ancestor_poll_interval(),
            DEFAULT_ANCESTOR_POLL_INTERVAL,
            "非法值回退默认"
        );
        unsafe { std::env::set_var("MEMSTACK_MCP_ANCESTOR_POLL_MS", " 250 ") };
        assert_eq!(
            resolve_ancestor_poll_interval(),
            Duration::from_millis(250),
            "允许首尾空白"
        );
        unsafe { std::env::remove_var("MEMSTACK_MCP_ANCESTOR_POLL_MS") };
    }
}
