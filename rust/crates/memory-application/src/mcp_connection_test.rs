//! MCP 连接测试服务：移植 C# `McpConnectionTestService`（stdio 形态，决策 9）。
//!
//! 与 C# 差异（架构决策 13）：不走固定 10212 HTTP 端口，而是 spawn 本机 MCP exe
//! 执行真实 stdio 握手（initialize + tools/list）：
//! - 校验 initialize 响应的协议版本与服务器信息；
//! - 校验 tools/list 恰好包含固定 21 个工具（16 个既有 + 5 个项目文档/结论卡片）；
//! - 报告成功/失败与总耗时；任一环节失败或超过超时（默认 10s）即终止子进程。

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 期望的协议协商版本（与 stdio 服务 initialize 响应一致）。
pub const EXPECTED_PROTOCOL_VERSION: &str = "2025-06-18";

/// 固定的 21 个工具名（16 个既有工具 + 5 个项目文档/结论卡片工具）。
pub const EXPECTED_TOOL_NAMES: [&str; 21] = [
    "memory_search",
    "memory_get",
    "memory_recent",
    "memory_context",
    "memory_related",
    "memory_create",
    "memory_update",
    "memory_archive",
    "memory_candidate_submit",
    "memory_candidate_list",
    "memory_candidate_confirm",
    "memory_candidate_reject",
    "project_list",
    "project_resolve",
    "project_create",
    "project_update",
    "project_handoff_get",
    "project_document_draft_create",
    "project_document_draft_update",
    "project_document_batch_update",
    "conclusion_card_candidate_submit",
];

/// 默认整体超时（对齐 C# HttpClient.Timeout = 10s）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// 连接测试结果（对齐 C# `McpConnectionTestResult`：成功标记 + 中文消息 + 工具数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpConnectionTestReport {
    /// 握手是否完整通过。
    pub ok: bool,
    /// 面向桌面的中文结论（成功：「MCP 连接测试通过」）。
    pub message: String,
    /// tools/list 实际返回的工具数量（失败时为已读到的数量，默认 0）。
    pub tool_count: usize,
    /// 从 spawn 到结论的总耗时（毫秒）。
    pub elapsed_ms: u128,
}

/// 对指定 MCP exe 执行 stdio 握手验收（默认 10s 超时）。
///
/// `extra_arguments`：透传给子进程的命令行参数（如 `--session-id <guid>`）；
/// 身份优先 `--session-id`（主路径），否则注入 `MEMSTACK_TOKEN` 环境变量。
pub fn test_mcp_connection(
    exe_path: &Path,
    database_path: &Path,
    plain_token: &str,
    extra_arguments: &[String],
) -> McpConnectionTestReport {
    test_mcp_connection_with_timeout(exe_path, database_path, plain_token, extra_arguments, DEFAULT_TIMEOUT)
}

/// 带显式超时的连接测试（单测注入假 exe 时使用短超时）。
pub fn test_mcp_connection_with_timeout(
    exe_path: &Path,
    database_path: &Path,
    plain_token: &str,
    extra_arguments: &[String],
    timeout: Duration,
) -> McpConnectionTestReport {
    let started = Instant::now();
    let report = |ok, message: String, tool_count| McpConnectionTestReport {
        ok,
        message,
        tool_count,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let mut command = Command::new(exe_path);
    command
        .env("MEMSTACK_DB_PATH", database_path)
        .args(extra_arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // GUI 进程 spawn 控制台程序时 Windows 会新建控制台窗口（用户看到的黑窗口），
    // CREATE_NO_WINDOW 抑制窗口创建，stdio 管道不受影响。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let uses_session_id = extra_arguments.iter().any(|argument| argument == "--session-id");
    if !uses_session_id {
        command.env("MEMSTACK_TOKEN", plain_token);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return report(false, format!("启动 MCP 进程失败：{error}"), 0),
    };
    let outcome = handshake(&mut child, timeout);
    // 无论成败都等待子进程退出（超时被杀时回收句柄），失败信息由握手给出。
    let _ = child.wait();
    match outcome {
        Ok(tool_count) => report(true, "MCP 连接测试通过".to_string(), tool_count),
        Err(message) => report(false, message, 0),
    }
}

/// 执行 initialize + tools/list 两帧握手；失败时确保子进程被终止。
fn handshake(child: &mut Child, timeout: Duration) -> Result<usize, String> {
    let deadline = Instant::now() + timeout;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "MCP 进程未提供标准输出".to_string())?;
    let (sender, receiver) = mpsc::channel::<String>();
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

    let result = (|| -> Result<usize, String> {
        let mut stdin = child.stdin.take().ok_or_else(|| "MCP 进程未提供标准输入".to_string())?;
        // initialize（客户端信息与 C# 桌面端一致）。
        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"memstack-desktop","version":"0.4.0"}}}"#;
        writeln!(stdin, "{initialize}")
            .and_then(|_| stdin.flush())
            .map_err(|error| format!("写入 initialize 失败：{error}"))?;
        let initialize_response = read_frame(&receiver, deadline, "initialize")?;
        verify_initialize(&initialize_response)?;

        // tools/list。
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
        )
        .and_then(|_| stdin.flush())
        .map_err(|error| format!("写入 tools/list 失败：{error}"))?;
        let tools_response = read_frame(&receiver, deadline, "tools/list")?;
        verify_tools(&tools_response)
    })();

    if result.is_err() {
        let _ = child.kill();
    }
    result
}

/// 在截止时间前读取一行 JSON-RPC 响应帧。
fn read_frame(receiver: &mpsc::Receiver<String>, deadline: Instant, stage: &str) -> Result<serde_json::Value, String> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| format!("MCP {stage} 响应超时"))?;
    let line = receiver
        .recv_timeout(remaining)
        .map_err(|_| format!("MCP {stage} 响应超时"))?;
    serde_json::from_str(&line).map_err(|error| format!("MCP {stage} 响应不是合法 JSON：{error}"))
}

/// 校验 initialize 响应：JSON-RPC result 存在且协议版本匹配。
fn verify_initialize(response: &serde_json::Value) -> Result<(), String> {
    let result = response
        .get("result")
        .ok_or_else(|| "MCP 协议调用返回错误".to_string())?;
    let version = result
        .get("protocolVersion")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "MCP initialize 响应缺少协议版本".to_string())?;
    if version != EXPECTED_PROTOCOL_VERSION {
        return Err(format!(
            "MCP 协议版本不匹配：期望 {EXPECTED_PROTOCOL_VERSION}，实际 {version}"
        ));
    }
    Ok(())
}

/// 校验 tools/list 响应：恰好 21 个工具且名称集合与固定清单一致。
fn verify_tools(response: &serde_json::Value) -> Result<usize, String> {
    let result = response
        .get("result")
        .ok_or_else(|| "MCP 协议调用返回错误".to_string())?;
    let tools = result
        .get("tools")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "MCP tools/list 响应缺少工具清单".to_string())?;
    let mut actual: Vec<String> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(|name| name.as_str()))
        .map(|name| name.to_string())
        .collect();
    actual.sort();
    let mut expected: Vec<&str> = EXPECTED_TOOL_NAMES.to_vec();
    expected.sort();
    if actual != expected {
        return Err("MCP 工具清单与固定的 21 个工具不一致".to_string());
    }
    Ok(tools.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    /// 生成回放两帧响应的假 exe 脚本，返回 (cmd, ["/c", 脚本路径])。
    /// `tag` 用于隔离并行测试的脚本目录。
    fn fake_replay_exe(tag: &str, initialize_frame: &str, tools_frame: &str) -> (String, Vec<String>) {
        let cmd = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
        let script_directory = std::env::temp_dir().join(format!("memstack-conn-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&script_directory).unwrap();
        let script_path = script_directory.join("fake-mcp.cmd");
        let mut script = std::fs::File::create(&script_path).unwrap();
        writeln!(script, "@echo off").unwrap();
        writeln!(script, "echo {initialize_frame}").unwrap();
        writeln!(script, "echo {tools_frame}").unwrap();
        drop(script);
        // 路径裸传（不手工加引号）：std 的 MS 参数转义会正确处理空格，
        // 手工引号会被转成 \" 导致 cmd 无法解析脚本路径。
        (cmd, vec!["/c".to_string(), script_path.to_string_lossy().into_owned()])
    }

    /// 成功握手帧：initialize（合法协议版本）+ tools/list（固定 21 工具）。
    fn success_frames() -> (String, String) {
        let initialize_frame = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"memstack","version":"0.4.0"}}}"#;
        let tools_frame = format!(
            r#"{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{}]}}}}"#,
            EXPECTED_TOOL_NAMES
                .iter()
                .map(|name| format!(r#"{{"name":"{name}"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        (initialize_frame.to_string(), tools_frame)
    }

    #[test]
    fn success_path_reports_ok_with_twenty_one_tools() {
        let database = repository_root().join("testdata/db-samples/full-sample.db");
        if !database.exists() {
            eprintln!("跳过：缺少 full-sample.db");
            return;
        }
        let (initialize_frame, tools_frame) = success_frames();
        let (cmd, arguments) = fake_replay_exe("ok", &initialize_frame, &tools_frame);
        let report = test_mcp_connection_with_timeout(
            Path::new(&cmd),
            &database,
            "unused-token",
            &arguments,
            Duration::from_secs(15),
        );
        assert!(report.ok, "成功路径应通过：{}", report.message);
        assert_eq!(report.tool_count, 21);
        assert_eq!(report.message, "MCP 连接测试通过");
    }

    #[test]
    fn unresponsive_process_times_out_and_is_killed() {
        let database = repository_root().join("testdata/db-samples/full-sample.db");
        if !database.exists() {
            eprintln!("跳过：缺少 full-sample.db");
            return;
        }
        let cmd = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
        let started = Instant::now();
        // ping 静默 30s：确保超过测试超时（2s）而不再输出任何行。
        let report = test_mcp_connection_with_timeout(
            Path::new(&cmd),
            &database,
            "unused-token",
            &["/c".to_string(), "ping -n 30 127.0.0.1 >nul".to_string()],
            Duration::from_secs(2),
        );
        eprintln!(
            "[timeout-test] ok={} message={} elapsed_ms={} wall={}ms",
            report.ok,
            report.message,
            report.elapsed_ms,
            started.elapsed().as_millis()
        );
        assert!(!report.ok);
        assert!(report.message.contains("超时"), "消息应为超时：{}", report.message);
        assert!(started.elapsed() < Duration::from_secs(10), "必须及时杀掉子进程");
    }

    #[test]
    fn wrong_tool_list_fails_with_fixed_message() {
        let database = repository_root().join("testdata/db-samples/full-sample.db");
        if !database.exists() {
            eprintln!("跳过：缺少 full-sample.db");
            return;
        }
        let initialize_frame = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#;
        let tools_frame = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"memory_search"}]}}"#;
        let (cmd, arguments) = fake_replay_exe("wrong", initialize_frame, tools_frame);
        let report = test_mcp_connection_with_timeout(
            Path::new(&cmd),
            &database,
            "unused-token",
            &arguments,
            Duration::from_secs(15),
        );
        assert!(!report.ok);
        assert_eq!(report.message, "MCP 工具清单与固定的 21 个工具不一致");
    }
}
