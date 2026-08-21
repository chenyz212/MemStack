//! T11 并发验收：5 个 MCP 进程同库并发执行「读 + 写 + 候选 + 项目」混合脚本。
//!
//! 断言（对应第三轮计划 T11-1）：
//! - 无 SQLITE_BUSY 泄漏：全部写操作成功（busy_timeout + 排队消化竞争；
//!   若耗尽会表现为 isError）；
//! - 无跨项目越权：项目绑定 Token 写个人范围必须被拒（isError）；
//! - 数据终态一致：memory / memory_candidate 新增行数与脚本成功数严格一致。

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn exe_path() -> PathBuf {
    env!("CARGO_BIN_EXE_MemStack-MCP").into()
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 复制样本库并为项目绑定 Token 补一行（复用 mcp_contract 的固定身份）。
fn prepare_database(work_directory: &std::path::Path) -> Option<(PathBuf, String, String)> {
    let sample = repo_root().join("testdata/db-samples/full-sample.db");
    let token_file = repo_root().join("testdata/golden/sample-token.json");
    if !sample.exists() || !token_file.exists() {
        eprintln!("跳过：缺少 full-sample.db 或 sample-token.json");
        return None;
    }
    std::fs::create_dir_all(work_directory).unwrap();
    let database_path = work_directory.join("concurrency.db");
    std::fs::copy(&sample, &database_path).unwrap();
    let token: Value = serde_json::from_str(&std::fs::read_to_string(&token_file).unwrap()).unwrap();
    let full_token = token["plainToken"].as_str().unwrap().to_string();

    let project_token = "uam_contract_project_0002";
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute(
            "INSERT INTO mcp_token( \
                 id,name,token_hash,access_mode,project_scope_json,expires_at,revoked_at,created_at, \
                 assistant_type,display_name,token_prefix,token_ciphertext,permission,project_id,last_used_at) \
             VALUES('99999999-2222-4222-8222-222222222222','project-concurrency',?1,'Api','[]',NULL,NULL, \
                 '2026-08-15T00:00:00.0000000+00:00','Generic','项目并发客户端','uam_pj','','ReadWrite', \
                 '11111111-1111-1111-1111-111111111111',NULL);",
            [sha256_hex(project_token)],
        )
        .unwrap();
    drop(connection);
    Some((database_path, full_token, project_token.to_string()))
}

struct SessionOutput {
    lines: Vec<String>,
    code: Option<i32>,
}

/// 一次性写入全部帧后关闭 stdin，收集全部 stdout 行（Rust exe 逐行同步处理）。
fn run_frames(database: &std::path::Path, token: &str, frames: &[String]) -> SessionOutput {
    let mut child = Command::new(exe_path())
        .env("MEMSTACK_DB_PATH", database)
        .env("MEMSTACK_TOKEN", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
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
    SessionOutput {
        lines: String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        code: output.status.code(),
    }
}

/// 全量 Token 工作脚本：读（recent/search/candidate_list/project_list）+ 写（create/submit）。
fn full_worker_script(worker: usize) -> Vec<String> {
    let unique = format!("并发工作进程 {worker}");
    vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"concurrency","version":"0"}}}"#.into(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_recent","arguments":{"limit":3}}}"#.into(),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"迁移","memoryType":"","tag":"","limit":3,"semanticEnabled":false}}}"#.into(),
        format!(r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"memory_create","arguments":{{"scope":"Personal","projectId":null,"title":"{unique}","summary":"并发写入","content":"并发正文 {unique}","memoryType":"NOTE","keywords":["并发"],"tags":[],"importance":3,"cloudProcessingAllowed":false}}}}}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{{"name":"memory_candidate_submit","arguments":{{"scope":"Personal","projectId":null,"title":"候选 {unique}","summary":"","content":"并发候选正文 {unique}","memoryType":"NOTE","keywords":[],"tags":[],"importance":3,"cloudProcessingAllowed":false}}}}}}"#),
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"memory_candidate_list","arguments":{}}}"#.into(),
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"project_list","arguments":{}}}"#.into(),
    ]
}

/// 项目 Token 工作脚本：个人写入必须被拒（无跨项目越权），项目内写入成功。
fn project_worker_script() -> Vec<String> {
    vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"concurrency","version":"0"}}}"#.into(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_create","arguments":{"scope":"Personal","projectId":null,"title":"越权尝试","summary":"","content":"项目 Token 不得写个人范围","memoryType":"NOTE","keywords":[],"tags":[],"importance":3,"cloudProcessingAllowed":false}}}"#.into(),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_create","arguments":{"scope":"Project","projectId":"11111111-1111-1111-1111-111111111111","title":"项目内合法写入","summary":"","content":"项目 Token 在绑定项目内写入","memoryType":"NOTE","keywords":[],"tags":[],"importance":3,"cloudProcessingAllowed":false}}}"#.into(),
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"project_list","arguments":{}}}"#.into(),
    ]
}

fn is_error(response: &Value) -> bool {
    response["result"]["isError"].as_bool().unwrap_or(false)
        || !response.get("error").map(|error| error.is_null()).unwrap_or(true)
}

#[test]
fn five_concurrent_processes_complete_without_busy_or_scope_leak() {
    let work_directory = std::env::temp_dir().join(format!("memstack-mcp-conc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work_directory);
    let Some((database, full_token, project_token)) = prepare_database(&work_directory) else {
        return;
    };

    let initial_memory: i64 = {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .query_row("SELECT count(*) FROM memory;", [], |row| row.get(0))
            .unwrap()
    };
    let initial_candidates: i64 = {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .query_row("SELECT count(*) FROM memory_candidate;", [], |row| row.get(0))
            .unwrap()
    };

    // 5 进程并发：4 个全量 Token + 1 个项目绑定 Token。
    let mut workers: Vec<std::thread::JoinHandle<SessionOutput>> = Vec::new();
    let database_arc = std::sync::Arc::new(database.clone());
    for worker in 0..4 {
        let database = std::sync::Arc::clone(&database_arc);
        let token = full_token.clone();
        let frames = full_worker_script(worker);
        workers.push(std::thread::spawn(move || run_frames(&database, &token, &frames)));
    }
    {
        let database = std::sync::Arc::clone(&database_arc);
        let token = project_token.clone();
        let frames = project_worker_script();
        workers.push(std::thread::spawn(move || run_frames(&database, &token, &frames)));
    }
    let sessions: Vec<SessionOutput> = workers.into_iter().map(|handle| handle.join().unwrap()).collect();

    // 1) 全部进程干净退出，帧数与请求一致。
    for (index, session) in sessions.iter().enumerate() {
        assert_eq!(session.code, Some(0), "进程 {index} 必须干净退出");
        let expected = if index < 4 { 7 } else { 4 };
        assert_eq!(
            session.lines.len(),
            expected,
            "进程 {index} 响应帧数不符：{:?}",
            session.lines
        );
    }

    // 2) 无 BUSY 泄漏：全部写操作（create/submit）成功。
    for (index, session) in sessions.iter().enumerate().take(4) {
        for response in &session.lines {
            let value: Value = serde_json::from_str(response).unwrap();
            assert!(
                !is_error(&value),
                "全量进程 {index} 不应出现任何错误响应（含 BUSY）：{response}"
            );
        }
    }

    // 3) 无跨项目越权：项目进程个人写入被拒，项目内写入成功，项目列表仅绑定项目。
    let project_session = &sessions[4];
    let denial: Value = serde_json::from_str(&project_session.lines[1]).unwrap();
    assert!(
        is_error(&denial),
        "项目 Token 写个人范围必须被拒：{}",
        project_session.lines[1]
    );
    let allowed: Value = serde_json::from_str(&project_session.lines[2]).unwrap();
    assert!(
        !is_error(&allowed),
        "项目 Token 在绑定项目内写入必须成功：{}",
        project_session.lines[2]
    );
    let projects: Value = serde_json::from_str(&project_session.lines[3]).unwrap();
    let project_ids: Vec<&str> = projects["result"]["structuredContent"]["result"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert_eq!(project_ids, vec!["11111111-1111-1111-1111-111111111111"]);

    // 4) 终态一致：memory +5（4 全量 + 1 项目合法），candidate +4。
    let connection = rusqlite::Connection::open(&database).unwrap();
    let final_memory: i64 = connection
        .query_row("SELECT count(*) FROM memory;", [], |row| row.get(0))
        .unwrap();
    let final_candidates: i64 = connection
        .query_row("SELECT count(*) FROM memory_candidate;", [], |row| row.get(0))
        .unwrap();
    assert_eq!(final_memory, initial_memory + 5, "记忆新增数必须等于成功写入数");
    assert_eq!(final_candidates, initial_candidates + 4, "候选新增数必须等于成功提交数");
    // FTS 与记忆行同步。
    let fts: i64 = connection
        .query_row("SELECT count(*) FROM memory_fts;", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fts, final_memory, "FTS 必须与记忆行同步");

    let _ = std::fs::remove_dir_all(&work_directory);
}
