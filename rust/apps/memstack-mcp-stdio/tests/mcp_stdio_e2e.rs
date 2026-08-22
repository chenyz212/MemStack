//! MemStack-MCP stdio 端到端验证：完整会话后 stdout 必须全部是合法 JSON-RPC，
//! tools/list 精确返回 memory_search，中文检索命中样本数据，EOF 干净退出。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn exe_path() -> PathBuf {
    env!("CARGO_BIN_EXE_MemStack-MCP").into()
}

struct Fixture {
    database_path: PathBuf,
    plain_token: String,
    work_directory: PathBuf,
}

fn setup_fixture() -> Option<Fixture> {
    let sample = repo_root().join("testdata/db-samples/full-sample.db");
    let token_file = repo_root().join("testdata/golden/sample-token.json");
    if !sample.exists() || !token_file.exists() {
        eprintln!("跳过：缺少 full-sample.db 或 sample-token.json");
        return None;
    }
    let thread_id = format!("{:?}", std::thread::current().id());
    let work_directory = std::env::temp_dir().join(format!("memstack-mcp-e2e-{}-{thread_id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work_directory);
    std::fs::create_dir_all(&work_directory).unwrap();
    let database_path = work_directory.join("memory.db");
    std::fs::copy(&sample, &database_path).unwrap();
    let token: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&token_file).unwrap()).unwrap();
    Some(Fixture {
        database_path,
        plain_token: token["plainToken"].as_str().unwrap().to_string(),
        work_directory,
    })
}

/// 启动子进程并执行一次完整 MCP 会话，返回 (stdout 行, stderr, 退出码)。
fn run_session(fixture: &Fixture, token: &str, frames: &[&str]) -> (Vec<String>, String, Option<i32>) {
    let mut child = Command::new(exe_path())
        .env("MEMSTACK_DB_PATH", &fixture.database_path)
        .env("MEMSTACK_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 MemStack-MCP 失败");
    {
        let stdin = child.stdin.as_mut().unwrap();
        for frame in frames {
            writeln!(stdin, "{frame}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("等待子进程失败");
    let stdout_lines = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (stdout_lines, stderr, output.status.code())
}

#[test]
fn full_session_keeps_stdout_pure_and_returns_search_results() {
    let Some(fixture) = setup_fixture() else { return };
    let (lines, stderr, code) = run_session(
        &fixture,
        &fixture.plain_token,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0.0.1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"迁移基线","memoryType":"","tag":"","limit":10,"semanticEnabled":false}}}"#,
        ],
    );

    // stdout 纯净性：每行都必须是合法 JSON-RPC。
    assert_eq!(code, Some(0), "EOF 后必须以 0 退出；stderr：{stderr}");
    assert_eq!(lines.len(), 3, "三条请求各一条响应；实际：{lines:?}");
    let initialize: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(initialize["result"]["serverInfo"]["name"], "memstack");
    assert_eq!(initialize["result"]["protocolVersion"], "2025-06-18");

    let tools_list: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    let tools = tools_list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 21, "必须注册 21 个工具（16 既有 + 5 项目文档）");
    assert!(
        tools.iter().any(|tool| tool["name"] == "memory_search"),
        "必须包含 memory_search"
    );
    assert!(
        tools.iter().any(|tool| tool["name"] == "project_handoff_get"),
        "必须包含 project_handoff_get"
    );

    let tools_call: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
    // structuredContent 与 C# 契约 outputSchema 一致：{"result": [...]} 对象包装。
    let structured = &tools_call["result"]["structuredContent"];
    assert!(
        structured.is_object(),
        "structuredContent 必须是对象（契约 outputSchema）"
    );
    let results = structured["result"].as_array().expect("result 字段必须是数组");
    assert!(!results.is_empty(), "中文检索必须命中样本数据");
    assert!(results[0]["memory"]["title"].as_str().unwrap().contains("基线"));

    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

/// MCP 规范：ping 须回空对象 {}，且会话存活（不发 -32601）。
#[test]
fn ping_returns_empty_result_object_and_session_survives() {
    let Some(fixture) = setup_fixture() else { return };
    let (lines, _stderr, code) = run_session(
        &fixture,
        &fixture.plain_token,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0.0.1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#,
        ],
    );
    assert_eq!(code, Some(0));
    let ping: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert!(ping["error"].is_null(), "ping 不得返回 JSON-RPC error");
    assert_eq!(ping["result"], serde_json::json!({}), "ping 必须回空对象");
    // 会话存活：ping 之后 tools/list 正常。
    let tools_list: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
    assert_eq!(tools_list["result"]["tools"].as_array().unwrap().len(), 21);
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

#[test]
fn invalid_token_exits_nonzero() {
    let Some(fixture) = setup_fixture() else { return };
    let (lines, stderr, code) = run_session(&fixture, "uam_invalid_token_x", &[]);
    assert_ne!(code, Some(0), "无效 Token 必须非 0 退出");
    assert!(lines.is_empty(), "鉴权失败不得写出协议消息");
    assert!(stderr.contains("失败"), "stderr 必须有故障摘要");
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

#[test]
fn unknown_tool_returns_jsonrpc_error_without_dying() {
    let Some(fixture) = setup_fixture() else { return };
    let (lines, _stderr, code) = run_session(
        &fixture,
        &fixture.plain_token,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0.0.1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_delete","arguments":{}}}"#,
        ],
    );
    assert_eq!(code, Some(0));
    let error_response: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    // 与 C# SDK 实测一致：未知工具 → -32602 "Unknown tool: '<name>'"。
    assert_eq!(error_response["error"]["code"].as_i64().unwrap(), -32602);
    assert_eq!(
        error_response["error"]["message"].as_str().unwrap(),
        "Unknown tool: 'memory_delete'"
    );
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

#[test]
fn business_error_returns_is_error_result_and_session_survives() {
    let Some(fixture) = setup_fixture() else { return };
    let (lines, _stderr, code) = run_session(
        &fixture,
        &fixture.plain_token,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0.0.1"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_get","arguments":{"memoryId":"00000000-0000-4000-8000-000000000000"}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"迁移基线","memoryType":"","tag":"","limit":5,"semanticEnabled":false}}}"#,
        ],
    );
    assert_eq!(code, Some(0));
    // 业务错误形态（修复后）：result.isError=true + 「[错误码] 中文消息（调用工具：xxx）」
    let error_frame: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    assert!(error_frame["error"].is_null(), "业务错误不得走 JSON-RPC error");
    assert_eq!(error_frame["result"]["isError"], true);
    let error_text = error_frame["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        error_text.starts_with("[MEMORY_NOT_FOUND]"),
        "content 以 [MEMORY_NOT_FOUND] 开头，实际: {error_text}"
    );
    assert!(
        error_text.contains("记忆不存在"),
        "content 含中文原因描述，实际: {error_text}"
    );
    assert!(
        error_text.ends_with("（调用工具：memory_get）"),
        "content 含工具名，实际: {error_text}"
    );
    // 不得携带 structuredContent：outputSchema 描述的是成功载荷（id/scope/…），
    // 错误详情放进 structuredContent 会被 MCP 客户端按 outputSchema 校验整体拒绝，
    // 真实错误消息到不了 LLM（实测曾把 MCP_PROJECT_SCOPE_DENIED 伪装成
    // 「Structured content does not match the tool's output schema」）。
    assert!(
        error_frame["result"].get("structuredContent").is_none(),
        "业务错误不得携带 structuredContent，实际：{}",
        error_frame["result"]
    );
    // 会话存活：后续调用正常。
    let ok_frame: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
    assert!(
        !ok_frame["result"]["structuredContent"]["result"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

#[test]
fn void_tool_returns_empty_content() {
    let Some(fixture) = setup_fixture() else { return };
    let (lines, _stderr, code) = run_session(
        &fixture,
        &fixture.plain_token,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0.0.1"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_candidate_reject","arguments":{"candidateId":"cccccccc-0000-0000-0000-000000000001","expectedVersion":1}}}"#,
        ],
    );
    assert_eq!(code, Some(0));
    let frame: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    // void 工具形态（实测 C# SDK）：content 为空数组，无 structuredContent。
    assert_eq!(frame["result"]["content"].as_array().unwrap().len(), 0);
    assert!(frame["result"].get("structuredContent").is_none() || frame["result"]["structuredContent"].is_null());
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

/// 在样本副本上为指定会话签发一条 DPAPI 密文 Token（模拟桌面端生成流程）。
fn issue_session_token(database_path: &std::path::Path, session_id: &str, plain_token: &str, revoked: bool) {
    let connection = rusqlite::Connection::open(database_path).unwrap();
    connection
        .execute(
            "INSERT INTO mcp_client_session(id, client_key, display_name, client_version, transport, last_seen_at, call_count, created_at, updated_at) \
             VALUES(?1, ?1, '会话身份测试', '0.4.0', 'Stdio', NULL, 0, '2026-08-15T00:00:00.0000000+00:00', '2026-08-15T00:00:00.0000000+00:00');",
            [session_id],
        )
        .unwrap();
    let ciphertext = memory_platform::dpapi::protect(plain_token).unwrap();
    let digest = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(plain_token.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    connection
        .execute(
            "INSERT INTO mcp_token( \
                 id,name,token_hash,access_mode,project_scope_json,expires_at,revoked_at,created_at, \
                 assistant_type,display_name,token_prefix,token_ciphertext,permission,project_id,last_used_at,session_id) \
             VALUES('55555555-5555-4555-8555-555555555555','session-e2e',?1,'Api','[]',NULL,?2, \
                 '2026-08-15T00:00:00.0000000+00:00','Generic','会话身份测试','uam_ses',?3,'ReadWrite',NULL,NULL,?4);",
            rusqlite::params![
                digest,
                if revoked { Some("2026-08-15T00:00:00.0000000+00:00".to_string()) } else { None },
                ciphertext,
                session_id
            ],
        )
        .unwrap();
}

#[test]
fn session_id_mode_loads_identity_from_dpapi_ciphertext() {
    let Some(fixture) = setup_fixture() else { return };
    let session_id = "66666666-6666-4666-8666-666666666666";
    let plain_token = "uam_session_mode_plain_token_001";
    issue_session_token(&fixture.database_path, session_id, plain_token, false);

    let mut child = Command::new(exe_path())
        .arg("--session-id")
        .arg(session_id)
        .env("MEMSTACK_DB_PATH", &fixture.database_path)
        .env_remove("MEMSTACK_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{}}}}"#
        )
        .unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "session 模式必须正常服务");
    let first_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let tools_list: serde_json::Value = serde_json::from_str(&first_line).unwrap();
    assert_eq!(tools_list["result"]["tools"].as_array().unwrap().len(), 21);
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

#[test]
fn session_id_mode_fails_after_revocation_or_unknown_session() {
    let Some(fixture) = setup_fixture() else { return };
    let session_id = "77777777-7777-4777-8777-777777777777";
    let plain_token = "uam_session_mode_plain_token_002";
    issue_session_token(&fixture.database_path, session_id, plain_token, true);

    // 全部吊销 → 启动失败（新初始化必须失败）。
    let status = Command::new(exe_path())
        .arg("--session-id")
        .arg(session_id)
        .env("MEMSTACK_DB_PATH", &fixture.database_path)
        .env_remove("MEMSTACK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_ne!(status.code(), Some(0), "吊销后的会话必须拒绝启动");

    // 会话不存在 → 启动失败。
    let status = Command::new(exe_path())
        .arg("--session-id")
        .arg("88888888-8888-4888-8888-888888888888")
        .env("MEMSTACK_DB_PATH", &fixture.database_path)
        .env_remove("MEMSTACK_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_ne!(status.code(), Some(0), "未知会话必须拒绝启动");
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

#[test]
fn stdout_json_purity_streaming_check() {
    // 逐行流式校验：模拟客户端边发边读，确认没有非 JSON 行混入 stdout。
    let Some(fixture) = setup_fixture() else { return };
    let mut child = Command::new(exe_path())
        .env("MEMSTACK_DB_PATH", &fixture.database_path)
        .env("MEMSTACK_TOKEN", &fixture.plain_token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin_handle = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        let mut stdin = stdin_handle;
        let _ = writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{}}}}"#
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
        let _ = writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
        );
    });
    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);
    let mut responses = 0;
    for line in reader.lines() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<serde_json::Value>(&line)
            .unwrap_or_else(|error| panic!("stdout 出现非 JSON 行：{error}；行内容：{line}"));
        responses += 1;
    }
    let status = child.wait().unwrap();
    assert_eq!(responses, 2);
    assert_eq!(status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

/// T10 连接测试服务对真实 exe 的端到端握手：报告成功且 21 个工具齐全。
#[test]
fn connection_test_service_passes_against_real_exe() {
    let Some(fixture) = setup_fixture() else { return };
    let report = memory_application::mcp_connection_test::test_mcp_connection(
        &exe_path(),
        &fixture.database_path,
        &fixture.plain_token,
        &[],
    );
    assert!(report.ok, "真实 exe 握手应通过：{}", report.message);
    assert_eq!(report.tool_count, 21);
    assert_eq!(report.message, "MCP 连接测试通过");
    assert!(report.elapsed_ms > 0);
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}

/// 观测埋点回归：tools/call 的耗时日志写入 `MEMSTACK_MCP_LOG_FILE` 指定文件，
/// 成功与业务错误调用分别标记 outcome=ok / outcome=error；
/// 同时测试子进程（显式 MEMSTACK_DB_PATH）不写生产 mcp-stdio.log。
#[test]
fn tool_call_writes_observation_log_line() {
    let Some(fixture) = setup_fixture() else { return };
    let log_path = fixture.work_directory.join("observe.log");
    let mut child = Command::new(exe_path())
        .env("MEMSTACK_DB_PATH", &fixture.database_path)
        .env("MEMSTACK_TOKEN", &fixture.plain_token)
        .env("MEMSTACK_MCP_LOG_FILE", &log_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 MemStack-MCP 失败");
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"memory_search","arguments":{{"query":"迁移基线","memoryType":"","tag":"","limit":5,"semanticEnabled":false}}}}}}"#
        )
        .unwrap();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"memory_get","arguments":{{"memoryId":"00000000-0000-4000-8000-000000000000"}}}}}}"#
        )
        .unwrap();
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("等待子进程失败");
    assert_eq!(output.status.code(), Some(0));
    let log_text = std::fs::read_to_string(&log_path).unwrap_or_else(|error| panic!("观测日志文件应存在：{error}"));
    let ok_line = log_text
        .lines()
        .find(|line| line.contains("tool_call name=memory_search"))
        .unwrap_or_else(|| panic!("缺少 memory_search 观测行，实际：{log_text}"));
    assert!(ok_line.contains("outcome=ok"), "成功调用标记 ok：{ok_line}");
    assert!(ok_line.contains("elapsed_ms="), "耗时字段存在：{ok_line}");
    assert!(ok_line.contains("args_bytes="), "参数体积字段存在：{ok_line}");
    let error_line = log_text
        .lines()
        .find(|line| line.contains("tool_call name=memory_get"))
        .unwrap_or_else(|| panic!("缺少 memory_get 观测行，实际：{log_text}"));
    assert!(
        error_line.contains("outcome=error"),
        "业务错误调用标记 error：{error_line}"
    );
    let _ = std::fs::remove_dir_all(&fixture.work_directory);
}
