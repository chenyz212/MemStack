//! MemStack-MCP：无窗口的 MCP stdio 服务（阶段 5 全量 16 工具）。
//!
//! 进程规则（对应迁移文档 §8.1）：
//! - 不创建窗口、托盘和 HTTP 监听。
//! - stdout 只写 MCP 协议消息（JSON-RPC over NDJSON）。
//! - 日志写入 stderr 与 `%LOCALAPPDATA%\MemStack\logs\mcp-stdio.log`。
//! - stdin EOF 后正常退出（退出码 0）；鉴权失败以非 0 退出码退出。
//!
//! 工具分发与权限守卫由 `memory-mcp` 契约层承载（契约权威：contracts/mcp-tools-list.json）；
//! 错误响应形态（沿自 C# 时代实测快照，现为稳定行为契约）：
//! - 业务/参数错误 → result.isError=true + 固定英文摘要（不含错误码，会话存活）。
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

/// 协议版本与服务器信息（与 C# 0.4.0 initialize 响应一致）。
const PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "memstack";
const SERVER_TITLE: &str = "MemStack";
const SERVER_VERSION: &str = "0.4.0";
/// stdin 空闲超时：连续 5 分钟未收到任何消息时主动退出，防止 AI 客户端异常退出后遗留僵尸进程。
const STDIN_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

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
    memory_platform::log_rotation::rotate_logs();
    let database_path = resolve_database_path()?;
    append_log(&format!("database: {}", database_path.display()));
    // §17.1 首次运行备份：与桌面进程共用同一安全网（marker 幂等；显式
    // MEMSTACK_DB_PATH 的测试场景跳过）。失败即退出，不写 marker 待重试。
    if std::env::var("MEMSTACK_DB_PATH").map_or(true, |value| value.trim().is_empty()) {
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
    // 最近记忆活动记录器（v8）：仅成功读取工具与 memory_create 落库。
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

    serve_stdio(&caller, &services)
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
/// 增加 stdin 空闲超时机制：AI 客户端异常退出时 stdin 管道可能不被正确关闭，
/// 导致 MCP 进程永远阻塞在读取上变成僵尸进程。连续 `STDIN_IDLE_TIMEOUT`
/// 未收到任何消息时主动退出，日志中记录原因。
fn serve_stdio(caller: &McpCallerContext, services: &Services) -> Result<(), String> {
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

    loop {
        match rx.recv_timeout(STDIN_IDLE_TIMEOUT) {
            Ok(Ok(line)) => {
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
                append_log(&format!(
                    "stdin 空闲超时（{}s 无消息），AI 客户端可能已断开",
                    STDIN_IDLE_TIMEOUT.as_secs()
                ));
                eprintln!(
                    "[MemStack-MCP] stdin 空闲超时（{}s），准备退出",
                    STDIN_IDLE_TIMEOUT.as_secs()
                );
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
    if !memory_mcp::registry::is_known_tool(name) {
        // 与 C# SDK 实测一致：Unknown tool: '<name>'。
        return ToolOutcome::Failed(format!("Unknown tool: '{name}'"));
    }
    match memory_mcp::dispatch::dispatch(name, &params["arguments"], caller, services) {
        Ok(value) => ToolOutcome::Success(shape_tool_success(value)),
        Err(error) => ToolOutcome::Success(shape_tool_error(name, error)),
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

/// 业务/参数错误形态：isError 结果 + 「错误码 + 中文消息」的结构化摘要，
/// 让 AI 客户端能直接看到具体问题，不再只有模糊的 "An error occurred"。
fn shape_tool_error(name: &str, error: memory_domain::BusinessError) -> Value {
    let code = error.code.as_str();
    let message = &error.message;
    // 结构化详情带 code/message，同时 content/text 给 LLM 可读摘要。
    let structured = json!({
        "isError": true,
        "error": {
            "code": code,
            "message": message,
            "tool": name,
        }
    });
    json!({
        "content": [{
            "type": "text",
            "text": format!("[{code}] {message}（调用工具：{name}）"),
        }],
        "structuredContent": structured,
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

/// 追加日志到 `%LOCALAPPDATA%\MemStack\logs\mcp-stdio.log`；失败时静默忽略。
fn append_log(message: &str) {
    use std::io::Write as _;

    let Ok(logs_dir) = memory_platform::logs_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&logs_dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir.join("mcp-stdio.log"))
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let _ = writeln!(file, "[{timestamp}] [pid={}] {message}", std::process::id());
    }
}
