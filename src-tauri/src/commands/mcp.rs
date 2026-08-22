//! MCP 客户端管理 Commands（§5.8，11 个）+ AI 客户端注册（阶段 7 任务 6，2 个）。
//!
//! - 删除清单已执行（§5.8）：无 port-status / restart / 10212 状态 / desktopKey / fragment / 18461。
//! - `test_mcp_client`：spawn 本机 MCP exe 执行真实 stdio 握手（复用第三轮 T10 服务）。
//! - `get_mcp_connection`：页面顶部连接信息改 stdio 形态（无端口语义）。
//! - exe 解析：`MEMSTACK_MCP_EXE` env 优先 → 桌面 exe 同目录 `MemStack-MCP.exe`。

use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_application::client_registration::{self, PathHealthReport, RegistrationReport, StdioOutline};
use memory_application::mcp_connection_test::{self, EXPECTED_PROTOCOL_VERSION};
use memory_domain::{
    CreateMcpClientRequest, McpClientCard, McpClientSecret, McpConnectionInfo, UpdateMcpClientRequest,
};

/// MCP 连接测试结果（对齐 C# `McpConnectionTestResult`：success/message/toolCount）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConnectionTestResult {
    pub success: bool,
    pub message: String,
    pub tool_count: i64,
}

/// 列出全部动态客户端卡片。
#[tauri::command]
pub async fn list_mcp_clients(state: State<'_, AppState>) -> Result<Vec<McpClientCard>, CommandError> {
    list_mcp_clients_impl(&state)
}

/// 读取单个客户端卡片。
#[tauri::command]
pub async fn get_mcp_client(state: State<'_, AppState>, session_id: String) -> Result<McpClientCard, CommandError> {
    get_mcp_client_impl(&state, &session_id)
}

/// 创建客户端会话（返回含明文 Token 的安全视图，仅此一次展示）。
#[tauri::command]
pub async fn create_mcp_client(
    state: State<'_, AppState>,
    request: CreateMcpClientRequest,
) -> Result<McpClientSecret, CommandError> {
    create_mcp_client_impl(&state, &request)
}

/// 更新客户端会话（名称/权限/项目/有效期）。
#[tauri::command]
pub async fn update_mcp_client(
    state: State<'_, AppState>,
    session_id: String,
    request: UpdateMcpClientRequest,
) -> Result<McpClientCard, CommandError> {
    update_mcp_client_impl(&state, &session_id, &request)
}

/// 轮换会话 ID：新 GUID 替换旧 `--session-id`（旧值立即失效），会话属性与令牌保留。
#[tauri::command]
pub async fn rotate_mcp_client_session_id(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<McpClientCard, CommandError> {
    rotate_mcp_client_session_id_impl(&state, &session_id)
}

/// 吊销客户端令牌（卡片保留）。
#[tauri::command]
pub async fn revoke_mcp_client(state: State<'_, AppState>, session_id: String) -> Result<McpClientCard, CommandError> {
    revoke_mcp_client_impl(&state, &session_id)
}

/// 删除客户端会话（含全部 Token 与卡片）。
#[tauri::command]
pub async fn delete_mcp_client(state: State<'_, AppState>, session_id: String) -> Result<(), CommandError> {
    delete_mcp_client_impl(&state, &session_id)
}

/// 按需解密客户端令牌（离开连接页/失焦即清）。
#[tauri::command]
pub async fn reveal_mcp_client_secret(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<McpClientSecret, CommandError> {
    reveal_mcp_client_secret_impl(&state, &session_id)
}

/// 读取客户端连接状态摘要（callCount / lastSeenAt / status，无握手）。
#[tauri::command]
pub async fn check_mcp_client_connection(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<McpClientCard, CommandError> {
    check_mcp_client_connection_impl(&state, &session_id)
}

/// 对客户端当前令牌执行真实 stdio 握手验收（initialize + tools/list，10s 超时）。
#[tauri::command]
pub async fn test_mcp_client(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<McpConnectionTestResult, CommandError> {
    test_mcp_client_impl(&state, &session_id)
}

/// 本机 MCP 连接信息（stdio 形态：exe 路径即 endpoint，无端口语义）。
#[tauri::command]
pub async fn get_mcp_connection() -> Result<McpConnectionInfo, CommandError> {
    Ok(get_mcp_connection_impl())
}

/// 预览指定 AI 客户端的接入配置文本（不落盘）。
#[tauri::command]
pub async fn get_mcp_client_config_preview(
    state: State<'_, AppState>,
    session_id: String,
    client_type: String,
) -> Result<RegistrationReport, CommandError> {
    get_mcp_client_config_preview_impl(&state, &session_id, &client_type)
}

/// 注册（写入）指定 AI 客户端的 stdio MCP 配置（备份 → 结构化写入 → 验证）。
#[tauri::command]
pub async fn register_mcp_client_config(
    state: State<'_, AppState>,
    session_id: String,
    client_type: String,
) -> Result<RegistrationReport, CommandError> {
    register_mcp_client_config_impl(&state, &session_id, &client_type)
}

/// 检查客户端配置中的 command 是否指向当前 MCP exe（绿色版移动失效检测）。
#[tauri::command]
pub async fn get_mcp_client_path_health(client_type: String) -> Result<PathHealthReport, CommandError> {
    get_mcp_client_path_health_impl(&client_type)
}

pub fn list_mcp_clients_impl(state: &AppState) -> Result<Vec<McpClientCard>, CommandError> {
    Ok(state.access.list_clients()?)
}

pub fn get_mcp_client_impl(state: &AppState, session_id: &str) -> Result<McpClientCard, CommandError> {
    Ok(state.access.get_client(session_id)?)
}

pub fn create_mcp_client_impl(
    state: &AppState,
    request: &CreateMcpClientRequest,
) -> Result<McpClientSecret, CommandError> {
    Ok(state.access.create_client(request)?)
}

pub fn update_mcp_client_impl(
    state: &AppState,
    session_id: &str,
    request: &UpdateMcpClientRequest,
) -> Result<McpClientCard, CommandError> {
    Ok(state.access.update_client(session_id, request)?)
}

pub fn rotate_mcp_client_session_id_impl(state: &AppState, session_id: &str) -> Result<McpClientCard, CommandError> {
    Ok(state.access.rotate_client_session_id(session_id)?)
}

pub fn revoke_mcp_client_impl(state: &AppState, session_id: &str) -> Result<McpClientCard, CommandError> {
    Ok(state.access.revoke_client(session_id)?)
}

pub fn delete_mcp_client_impl(state: &AppState, session_id: &str) -> Result<(), CommandError> {
    Ok(state.access.delete_client(session_id)?)
}

pub fn reveal_mcp_client_secret_impl(state: &AppState, session_id: &str) -> Result<McpClientSecret, CommandError> {
    Ok(state.access.get_client_secret(session_id)?)
}

pub fn check_mcp_client_connection_impl(state: &AppState, session_id: &str) -> Result<McpClientCard, CommandError> {
    Ok(state.access.get_client(session_id)?)
}

pub fn test_mcp_client_impl(state: &AppState, session_id: &str) -> Result<McpConnectionTestResult, CommandError> {
    // 取明文 Token：顺带校验会话存在、未吊销、密文可解（与 C# GetSecretAsync 同规则）。
    let secret = state.access.get_client_secret(session_id)?;
    let exe = resolve_mcp_exe().ok_or_else(|| {
        CommandError::with_message(
            memory_domain::ErrorCode::InternalError,
            "未找到 MemStack-MCP.exe：请将其与桌面程序放在同一目录，或设置 MEMSTACK_MCP_EXE 环境变量",
        )
    })?;
    let arguments = vec!["--session-id".to_string(), session_id.to_string()];
    let report = mcp_connection_test::test_mcp_connection(&exe, &state.database_path, &secret.plain_token, &arguments);
    Ok(McpConnectionTestResult {
        success: report.ok,
        message: report.message,
        tool_count: report.tool_count as i64,
    })
}

pub fn get_mcp_connection_impl() -> McpConnectionInfo {
    match resolve_mcp_exe() {
        Some(exe) => McpConnectionInfo {
            endpoint: exe.to_string_lossy().into_owned(),
            protocol_version: EXPECTED_PROTOCOL_VERSION.to_string(),
            status: "READY".to_string(),
            error_message: None,
        },
        None => McpConnectionInfo {
            endpoint: String::new(),
            protocol_version: EXPECTED_PROTOCOL_VERSION.to_string(),
            status: "FAILED".to_string(),
            error_message: Some(
                "未找到 MemStack-MCP.exe：请将其与桌面程序放在同一目录，或设置 MEMSTACK_MCP_EXE 环境变量".to_string(),
            ),
        },
    }
}

pub fn get_mcp_client_config_preview_impl(
    state: &AppState,
    session_id: &str,
    client_type: &str,
) -> Result<RegistrationReport, CommandError> {
    let outline = registration_outline(state, session_id)?;
    let (home, appdata) = user_directories()?;
    Ok(client_registration::preview_config(
        &outline,
        client_type,
        &home,
        &appdata,
    )?)
}

pub fn register_mcp_client_config_impl(
    state: &AppState,
    session_id: &str,
    client_type: &str,
) -> Result<RegistrationReport, CommandError> {
    let outline = registration_outline(state, session_id)?;
    let (home, appdata) = user_directories()?;
    Ok(client_registration::register_config(
        &outline,
        client_type,
        &home,
        &appdata,
    )?)
}

pub fn get_mcp_client_path_health_impl(client_type: &str) -> Result<PathHealthReport, CommandError> {
    // 健康检测只看配置文件与 exe 路径，不触碰数据库。
    let exe = resolve_mcp_exe().ok_or_else(|| {
        CommandError::with_message(
            memory_domain::ErrorCode::InternalError,
            "未找到 MemStack-MCP.exe：请将其与桌面程序放在同一目录，或设置 MEMSTACK_MCP_EXE 环境变量",
        )
    })?;
    let (home, appdata) = user_directories()?;
    Ok(client_registration::check_path_health(
        client_type,
        &exe.to_string_lossy(),
        &home,
        &appdata,
    )?)
}

/// 组装 stdio 接入轮廓：exe 绝对路径 + 会话 GUID（会话必须存在且未吊销）。
fn registration_outline(state: &AppState, session_id: &str) -> Result<StdioOutline, CommandError> {
    // 校验会话有效（存在且有活动 Token）。
    state.access.get_client_secret(session_id)?;
    let exe = resolve_mcp_exe().ok_or_else(|| {
        CommandError::with_message(
            memory_domain::ErrorCode::InternalError,
            "未找到 MemStack-MCP.exe：请将其与桌面程序放在同一目录，或设置 MEMSTACK_MCP_EXE 环境变量",
        )
    })?;
    Ok(StdioOutline {
        command: exe.to_string_lossy().into_owned(),
        session_id: session_id.to_string(),
    })
}

/// 解析 MCP exe 路径：`MEMSTACK_MCP_EXE` env 优先 → 桌面 exe 同目录 `MemStack-MCP.exe`。
pub fn resolve_mcp_exe() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("MEMSTACK_MCP_EXE")
        && !explicit.trim().is_empty()
    {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe_directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let candidate = exe_directory.join("MemStack-MCP.exe");
    candidate.is_file().then_some(candidate)
}

/// 读取注册所需用户目录（%USERPROFILE% 与 %APPDATA%）。
fn user_directories() -> Result<(PathBuf, PathBuf), CommandError> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).ok_or_else(|| {
        CommandError::with_message(memory_domain::ErrorCode::InternalError, "未定义 USERPROFILE 环境变量")
    })?;
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).ok_or_else(|| {
        CommandError::with_message(memory_domain::ErrorCode::InternalError, "未定义 APPDATA 环境环境变量")
    })?;
    Ok((home, appdata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::{ErrorCode, McpPermission};

    /// 环境变量类测试的串行锁（避免并行 env 竞态）。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        (AppState::build(database_path).unwrap(), temp)
    }

    fn create_request(name: &str) -> CreateMcpClientRequest {
        CreateMcpClientRequest {
            display_name: name.to_string(),
            permission: McpPermission::ReadWrite,
            project_id: None,
            expires_at: None,
        }
    }

    #[test]
    fn mcp_client_lifecycle_covers_all_commands() {
        let (state, _temp) = state();
        assert!(list_mcp_clients_impl(&state).unwrap().is_empty());

        let secret = create_mcp_client_impl(&state, &create_request("Codex")).unwrap();
        assert!(!secret.plain_token.is_empty());
        let session_id = secret.client.session_id.clone();
        assert_eq!(list_mcp_clients_impl(&state).unwrap().len(), 1);

        let card = get_mcp_client_impl(&state, &session_id).unwrap();
        assert_eq!(card.display_name, "Codex");
        assert_eq!(card.call_count, 0);

        let updated = update_mcp_client_impl(
            &state,
            &session_id,
            &UpdateMcpClientRequest {
                display_name: "Codex CLI".to_string(),
                permission: McpPermission::Read,
                project_id: None,
                expires_at: None,
                clear_expires_at: true,
            },
        )
        .unwrap();
        assert_eq!(updated.display_name, "Codex CLI");
        assert_eq!(updated.permission, McpPermission::Read);

        // 轮换会话 ID：新 id 生效、旧 id 立即失效；属性与令牌保留。
        let rotated = rotate_mcp_client_session_id_impl(&state, &session_id).unwrap();
        assert_ne!(rotated.session_id, session_id);
        assert_eq!(rotated.display_name, "Codex CLI");
        assert_eq!(rotated.permission, McpPermission::Read);
        assert_eq!(rotated.token_prefix, secret.client.token_prefix);
        let error = get_mcp_client_impl(&state, &session_id).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::McpClientNotFound.as_str());
        let revealed = reveal_mcp_client_secret_impl(&state, &rotated.session_id).unwrap();
        assert_eq!(revealed.plain_token, secret.plain_token, "令牌不随会话 ID 轮换");
        let session_id = rotated.session_id;

        let checked = check_mcp_client_connection_impl(&state, &session_id).unwrap();
        assert_eq!(checked.status, memory_domain::McpClientStatus::Active);

        let revoked = revoke_mcp_client_impl(&state, &session_id).unwrap();
        assert_eq!(revoked.status, memory_domain::McpClientStatus::Revoked);
        // 吊销后解密与轮换被拒绝。
        let error = reveal_mcp_client_secret_impl(&state, &session_id).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::McpTokenRevoked.as_str());

        delete_mcp_client_impl(&state, &session_id).unwrap();
        assert!(list_mcp_clients_impl(&state).unwrap().is_empty());
        let error = get_mcp_client_impl(&state, &session_id).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::McpClientNotFound.as_str());
    }

    #[test]
    fn test_mcp_client_reports_failure_when_exe_missing() {
        let (state, _temp) = state();
        let secret = create_mcp_client_impl(&state, &create_request("Codex")).unwrap();
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::remove_var("MEMSTACK_MCP_EXE") };
        // current_exe 在测试进程下指向 cargo/test 二进制目录，无 MemStack-MCP.exe → 缺失路径。
        let error = test_mcp_client_impl(&state, &secret.client.session_id).unwrap_err();
        assert!(error.message.contains("未找到 MemStack-MCP.exe"));
    }

    #[test]
    fn get_mcp_connection_reports_stdio_shape() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::remove_var("MEMSTACK_MCP_EXE") };
        let info = get_mcp_connection_impl();
        assert_eq!(info.protocol_version, "2025-06-18");
        // 测试环境无 exe → FAILED + 中文提示；endpoint 无端口语义。
        assert_eq!(info.status, "FAILED");
        assert!(info.error_message.is_some());
        assert!(!info.endpoint.contains("10212"));
    }

    #[test]
    fn get_mcp_client_path_health_reports_matching_command() {
        let home = tempfile::tempdir().unwrap();
        let appdata = tempfile::tempdir().unwrap();
        let exe_directory = tempfile::tempdir().unwrap();
        let exe = exe_directory.path().join("MemStack-MCP.exe");
        std::fs::write(&exe, b"fake").unwrap();

        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe {
            std::env::set_var("USERPROFILE", home.path());
            std::env::set_var("APPDATA", appdata.path());
            std::env::set_var("MEMSTACK_MCP_EXE", &exe);
        }

        // 未注册：supported 但未配置。
        let health = get_mcp_client_path_health_impl("codex").unwrap();
        assert!(health.supported);
        assert!(!health.configured);

        // 注册后（直接落盘 Codex 配置，路径用 TOML 字面量字符串）→ healthy。
        let codex_dir = home.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            format!(
                "[mcp_servers.memstack]\ncommand = '{}'\nargs = [\"--session-id\", \"x\"]\n",
                exe.display()
            ),
        )
        .unwrap();
        let health = get_mcp_client_path_health_impl("codex").unwrap();
        assert!(health.configured);
        assert!(health.healthy, "配置指向当前 exe 应健康");

        // Generic：不参与检测。
        let generic = get_mcp_client_path_health_impl("trae").unwrap();
        assert!(!generic.supported);

        // SAFETY：同上。
        unsafe {
            std::env::remove_var("MEMSTACK_MCP_EXE");
            std::env::remove_var("USERPROFILE");
            std::env::remove_var("APPDATA");
        }
    }

    /// 假 exe 回放握手帧：复用 mcp_connection_test 测试基建思路（cmd /c 脚本）。
    #[test]
    fn test_mcp_client_handshakes_with_fake_exe() {
        let (state, _temp) = state();
        let secret = create_mcp_client_impl(&state, &create_request("Codex")).unwrap();
        let session_id = secret.client.session_id.clone();

        let script_directory = std::env::temp_dir().join(format!("memstack-cmd-test-{}", std::process::id()));
        std::fs::create_dir_all(&script_directory).unwrap();
        let script_path = script_directory.join("fake-mcp.cmd");
        let mut script = std::fs::File::create(&script_path).unwrap();
        use std::io::Write as _;
        writeln!(script, "@echo off").unwrap();
        writeln!(
            script,
            r#"echo {{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-06-18"}}}}"#
        )
        .unwrap();
        let tools: Vec<String> = mcp_connection_test::EXPECTED_TOOL_NAMES
            .iter()
            .map(|name| format!(r#"{{"name":"{name}"}}"#))
            .collect();
        writeln!(
            script,
            r#"echo {{"jsonrpc":"2.0","id":2,"result":{{"tools":[{}]}}}}"#,
            tools.join(",")
        )
        .unwrap();
        drop(script);

        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe { std::env::set_var("MEMSTACK_MCP_EXE", &script_path) };
        let result = test_mcp_client_impl(&state, &session_id).unwrap();
        // SAFETY：同上。
        unsafe { std::env::remove_var("MEMSTACK_MCP_EXE") };
        assert!(result.success, "假 exe 握手应通过：{}", result.message);
        assert_eq!(result.tool_count, 21);
        assert_eq!(result.message, "MCP 连接测试通过");
        // 假 exe 不访问数据库：连接活动不回填（真实回填由 stdio 服务认证时写入，
        // 见 mcp_stdio_e2e / mcp_connection_test 测试）。
        let card = check_mcp_client_connection_impl(&state, &session_id).unwrap();
        assert_eq!(card.call_count, 0, "假 exe 不应产生连接活动");
    }
}
