//! Rust MCP stdio e2e 契约测试：spawn 本 crate bin（`CARGO_BIN_EXE_MemStack-MCP`）
//! 逐帧驱动 JSON-RPC，验证 initialize 指令、工具清单 schema 与工作空间执行规范。
//!
//! 历史：本文件曾承载与 C# mcp-child 的跨语言逐帧对比（迁移验收 T9），
//! 迁移完成后 C# 侧已退役删除，仅保留 Rust 单侧行为回归。

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 复制 full-sample.db 并补三条固定 Token（读写 / 只读 / 项目绑定），
/// 供不同权限场景使用；缺样本时跳过（与 db-samples 跳过策略一致）。
fn prepare_database(work_directory: &std::path::Path) -> Option<(PathBuf, String, String, String)> {
    let sample = repo_root().join("testdata/db-samples/full-sample.db");
    if !sample.exists() {
        eprintln!("跳过：缺少 full-sample.db");
        return None;
    }
    std::fs::create_dir_all(work_directory).unwrap();
    let database_path = work_directory.join("contract.db");
    std::fs::copy(&sample, &database_path).unwrap();

    let read_only_token = "uam_contract_readonly_0001";
    let project_token = "uam_contract_project_0002";
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute(
            "INSERT INTO mcp_token( \
                 id,name,token_hash,access_mode,project_scope_json,expires_at,revoked_at,created_at, \
                 assistant_type,display_name,token_prefix,token_ciphertext,permission,project_id,last_used_at) \
             VALUES('99999999-1111-4111-8111-111111111111','readonly-e2e',?1,'Api','[]',NULL,NULL, \
                 '2026-08-15T00:00:00.0000000+00:00','Generic','只读契约客户端','uam_ro','','Read',NULL,NULL);",
            [sha256_hex(read_only_token)],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO mcp_token( \
                 id,name,token_hash,access_mode,project_scope_json,expires_at,revoked_at,created_at, \
                 assistant_type,display_name,token_prefix,token_ciphertext,permission,project_id,last_used_at) \
             VALUES('99999999-2222-4222-8222-222222222222','project-e2e',?1,'Api','[]',NULL,NULL, \
                 '2026-08-15T00:00:00.0000000+00:00','Generic','项目契约客户端','uam_pj','','ReadWrite', \
                 '11111111-1111-1111-1111-111111111111',NULL);",
            [sha256_hex(project_token)],
        )
        .unwrap();
    drop(connection);
    Some((
        database_path,
        read_only_token.to_string(),
        project_token.to_string(),
        "uam_b1b61a97b5a14e2f9c1d0e3a7f6c5b4a99887766554433221100ffeeddccbbaa".to_string(),
    ))
}

struct ChildSession {
    kind: &'static str,
    child: Child,
}

impl ChildSession {
    fn spawn(kind: &'static str, exe: &std::path::Path, database: &std::path::Path, token: &str) -> Self {
        let mut command = Command::new(exe);
        command
            .env("MEMSTACK_DB_PATH", database)
            .env("MEMSTACK_TOKEN", token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().expect("启动 MCP 子进程失败");
        Self { kind, child }
    }

    /// 逐帧请求-响应执行（lockstep）：每条带 id 的请求等待其响应行后再发下一条。
    /// 通知帧后停 350ms 等就绪；EOF 提前关闭会触发会话取消 ——
    /// 因此保持 stdin 打开直到全部请求完成。
    fn run(mut self, frames: &[String]) -> Vec<String> {
        let stdout = self.child.stdout.take().unwrap();
        let stderr = self.child.stderr.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr_handle = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut stderr = stderr;
            let mut buffer = String::new();
            let _ = stderr.read_to_string(&mut buffer);
            buffer
        });

        let mut responses = Vec::new();
        {
            let stdin = self.child.stdin.as_mut().unwrap();
            for frame in frames {
                writeln!(stdin, "{frame}").unwrap();
                stdin.flush().unwrap();
                let is_notification = serde_json::from_str::<Value>(frame)
                    .map(|value| value.get("id").is_none())
                    .unwrap_or(true);
                if is_notification {
                    std::thread::sleep(std::time::Duration::from_millis(350));
                } else {
                    let response = receiver
                        .recv_timeout(std::time::Duration::from_secs(20))
                        .unwrap_or_else(|error| panic!("[{}] 等待响应失败：{error}；帧：{frame}", self.kind));
                    responses.push(response);
                }
            }
        }
        drop(self.child.stdin.take());
        // 回收多余行（正常应无）。
        while let Ok(extra) = receiver.recv_timeout(std::time::Duration::from_millis(500)) {
            responses.push(extra);
        }
        let output = self.child.wait().unwrap();
        let stderr_text = stderr_handle.join().unwrap_or_default();
        if !stderr_text.trim().is_empty() {
            eprintln!("[{}] stderr：{}", self.kind, stderr_text);
        }
        assert_eq!(
            output.code(),
            Some(0),
            "[{}] 子进程必须干净退出；响应 {} 帧；stderr：{}",
            self.kind,
            responses.len(),
            stderr_text
        );
        responses
    }
}

/// Rust 单侧 e2e：工作空间首次绑定闭环回归。
/// initialize 携带新 instructions；tools/list 中 project_resolve 无必填参数；
/// 缺省参数 resolve（宿主 cwd=crate 目录，形如真实路径）→ 未绑定 →
/// PROJECT_NAME_REQUIRED + 询问中文项目名；显式标识 resolve 同样 PROJECT_NAME_REQUIRED；
/// AI 用用户答复的中文名 project_create 绑定同一标识后，再 resolve → MAPPED 且 projectId 一致。
#[test]
fn mcp_rust_workspace_flow() {
    let work = std::env::temp_dir().join(format!("memstack-ws-flow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let Some((database, _, _, full_token)) = prepare_database(&work) else {
        return;
    };
    let script = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"ws-flow","version":"0"}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#.to_string(),
        // 缺省参数：服务端采用宿主 cwd（crate 目录，形如真实路径）→ 未绑定 → 询问中文名。
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"project_resolve","arguments":{}}}"#.to_string(),
        // 显式标识（完整路径）：同样未绑定 → 询问中文名，返回归一化标识 flow-test。
        r##"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"project_resolve","arguments":{"workspaceIdentifier":"E:\\ws\\flow-test"}}}"##.to_string(),
        // 首次绑定闭环：AI 拿用户答复的中文名 + resolve 返回的标识创建项目。
        r##"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"project_create","arguments":{"name":"集成测试项目","description":"首次绑定闭环","color":"#238f7a","workspaceIdentifier":"flow-test"}}}"##.to_string(),
        // 创建后显式 resolve 同一标识 → MAPPED。
        r##"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"project_resolve","arguments":{"workspaceIdentifier":"flow-test"}}}"##.to_string(),
    ];
    let rust_exe = PathBuf::from(env!("CARGO_BIN_EXE_MemStack-MCP"));
    let responses = ChildSession::spawn("rust", &rust_exe, &database, &full_token).run(&script);
    assert_eq!(responses.len(), 6, "六帧请求各一响应");
    let initialize: Value = serde_json::from_str(&responses[0]).unwrap();
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("记忆保存执行规范")),
        "initialize instructions 应携带执行规范"
    );
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("PROJECT_NAME_REQUIRED")),
        "instructions 应说明 PROJECT_NAME_REQUIRED 处置方式"
    );
    let tools_list: Value = serde_json::from_str(&responses[1]).unwrap();
    let resolve_schema = &tools_list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "project_resolve")
        .unwrap()["inputSchema"];
    assert!(
        resolve_schema.get("required").is_none(),
        "project_resolve 工作空间标识应为可选参数"
    );
    // 帧 3：缺省参数（hint=crate 目录）→ 未绑定 → 询问中文项目名。
    let resolve_hint: Value = serde_json::from_str(&responses[2]).unwrap();
    let structured = &resolve_hint["result"]["structuredContent"];
    assert_eq!(
        structured["status"],
        json!("PROJECT_NAME_REQUIRED"),
        "未绑定工作空间应返回询问中文名状态"
    );
    assert!(
        structured["question"]
            .as_str()
            .is_some_and(|text| text.contains("中文项目名称")),
        "应返回询问中文项目名的文案"
    );
    assert_eq!(structured["requiresUserInput"], json!(true));
    // 帧 4：显式完整路径 → 未绑定，返回归一化标识。
    let resolve_explicit: Value = serde_json::from_str(&responses[3]).unwrap();
    let structured = &resolve_explicit["result"]["structuredContent"];
    assert_eq!(structured["status"], json!("PROJECT_NAME_REQUIRED"));
    assert_eq!(structured["workspaceIdentifier"], json!("flow-test"));
    // 帧 5：用户答复中文名 → 创建并绑定。
    let created: Value = serde_json::from_str(&responses[4]).unwrap();
    let structured = &created["result"]["structuredContent"];
    assert_eq!(structured["name"], json!("集成测试项目"));
    assert_eq!(structured["workspaceBound"], json!(true));
    assert_eq!(structured["workspaceIdentifier"], json!("flow-test"));
    let project_id = structured["id"].as_str().unwrap().to_string();
    // 帧 6：同标识再 resolve → MAPPED 到新项目。
    let remapped: Value = serde_json::from_str(&responses[5]).unwrap();
    let structured = &remapped["result"]["structuredContent"];
    assert_eq!(structured["status"], json!("MAPPED"));
    assert_eq!(structured["project"]["id"], json!(project_id));
    let _ = std::fs::remove_dir_all(&work);
}

/// Rust 单侧 e2e：错误路径契约（业务错误为 isError + 「错误码 中文消息」结构化摘要；未知工具/方法为 JSON-RPC -32602/-32601）。
#[test]
fn mcp_rust_error_shapes() {
    let work = std::env::temp_dir().join(format!("memstack-ws-errors-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    let Some((database, _, _, full_token)) = prepare_database(&work) else {
        return;
    };
    let script = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"err-shapes","version":"0"}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
        // 1. 记忆不存在 → 业务错误：isError + [MEMORY_NOT_FOUND] 中文摘要。
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"memory_get","arguments":{"memoryId":"00000000-0000-4000-8000-000000000000"}}}"#.to_string(),
        // 2. 参数缺失 → 业务错误（isError），同上。
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"memory_search","arguments":{"query":"迁移"}}}"#.to_string(),
        // 3. memoryType=procedure 非法枚举 → MEMORY_TYPE_INVALID。
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"memory_create","arguments":{"scope":"Personal","projectId":null,"title":"T","summary":"","content":"C","memoryType":"procedure","keywords":[],"tags":[],"importance":3,"cloudProcessingAllowed":false}}}"#.to_string(),
        // 4. importance=8 超界 → MEMORY_IMPORTANCE_INVALID。
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"memory_create","arguments":{"scope":"Personal","projectId":null,"title":"T","summary":"","content":"C","memoryType":"NOTE","keywords":[],"tags":[],"importance":8,"cloudProcessingAllowed":false}}}"#.to_string(),
        // 5. 未知工具 → JSON-RPC -32602。
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"memory_delete","arguments":{}}}"#.to_string(),
        // 6. 未知方法 → JSON-RPC -32601。
        r#"{"jsonrpc":"2.0","id":7,"method":"resources/list","params":{}}"#.to_string(),
    ];
    let rust_exe = PathBuf::from(env!("CARGO_BIN_EXE_MemStack-MCP"));
    let responses = ChildSession::spawn("rust", &rust_exe, &database, &full_token).run(&script);
    assert_eq!(responses.len(), 7, "七帧请求各一响应");
    // —— 前四帧业务错误：isError=true + content 文本以 "[XXX]" 形式带错误码开头 + structuredContent.error 含 code/message/tool ——
    let business_expectations: &[(usize, &str, &str); 4] = &[
        (1, "MEMORY_NOT_FOUND", "记忆不存在"),
        (2, "INTERNAL_ERROR", "参数解析失败"),
        (3, "MEMORY_TYPE_INVALID", "记忆类型必须是以下之一"),
        (4, "MEMORY_IMPORTANCE_INVALID", "重要程度必须为 1 到 5"),
    ];
    for &(index, expected_code, expected_msg_prefix) in business_expectations {
        let frame: Value = serde_json::from_str(&responses[index]).unwrap();
        assert_eq!(
            frame["result"]["isError"],
            json!(true),
            "第 {} 帧应为 isError",
            index + 1
        );
        let text = frame["result"]["content"][0]["text"].as_str().expect("content text");
        assert!(
            text.starts_with(&format!("[{expected_code}]")),
            "第 {} 帧 content 应以 [{expected_code}] 开头, 实际: {text}",
            index + 1
        );
        assert!(
            text.contains(expected_msg_prefix),
            "第 {} 帧 content 应包含中文摘要 '{expected_msg_prefix}'",
            index + 1
        );
        // 结构化详情也应有 code/message/tool。
        let structured_error = &frame["result"]["structuredContent"]["error"];
        assert_eq!(
            structured_error["code"],
            json!(expected_code),
            "第 {} 帧 structuredContent.error.code 一致",
            index + 1
        );
        assert!(
            structured_error["message"]
                .as_str()
                .is_some_and(|m| m.contains(expected_msg_prefix)),
            "第 {} 帧 structuredContent.error.message 含中文摘要",
            index + 1
        );
        assert!(
            structured_error["tool"].as_str().is_some_and(|t| !t.is_empty()),
            "第 {} 帧 structuredContent.error.tool 非空",
            index + 1
        );
    }
    let unknown_tool: Value = serde_json::from_str(&responses[5]).unwrap();
    assert_eq!(unknown_tool["error"]["code"], json!(-32602), "未知工具 → -32602");
    let unknown_method: Value = serde_json::from_str(&responses[6]).unwrap();
    assert_eq!(unknown_method["error"]["code"], json!(-32601), "未知方法 → -32601");
    let _ = std::fs::remove_dir_all(&work);
}
