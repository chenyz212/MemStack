//! 客户端配置注册：为 AI 客户端写入 stdio MCP 接入配置（总计划 §9.2）。
//!
//! - Codex：`~/.codex/config.toml` 用 toml_edit 结构化编辑（保留既有段落与注释，
//!   禁止字符串拼接改写 TOML）；写入前备份 `config.toml.bak.<yyyyMMdd-HHmmss>`，
//!   写入后重读验证 exe 路径与会话 GUID；目录不存在时自动创建 `~/.codex`。
//! - Claude Desktop：`%APPDATA%\Claude\claude_desktop_config.json`；Cursor：
//!   `%USERPROFILE%\.cursor\mcp.json`。两者用 serde_json 读改写（保留未知键），
//!   同样时间戳备份 + 重读验证。
//! - Trae / WorkBuddy / Qoder / 未知客户端：不支持自动写入，仅生成可复制的通用
//!   stdio 配置 JSON 文本（supported=false、written=false，不落盘、不伪装已连接）。
//! - 本 crate 不依赖 Tauri：exe 路径与会话 GUID 由 `StdioOutline` 传入，
//!   `%USERPROFILE%` / `%APPDATA%` 由调用方注入以便测试。

use std::path::{Path, PathBuf};

use memory_domain::{BusinessError, ErrorCode};
use toml_edit::{Array, DocumentMut, Item, Table, value};

use crate::mcp_access::normalize_client_key;

/// JSON 型客户端（Claude / Cursor / Generic）的 MCP 服务键名。
const JSON_SERVER_KEY: &str = "memstack";

/// Codex TOML 中的 MCP 服务键名（§9.2：`[mcp_servers.memstack]`）。
const CODEX_SERVER_KEY: &str = "memstack";

/// stdio 会话参数名。
const SESSION_ID_ARGUMENT: &str = "--session-id";

/// 通用客户端（不支持自动写入）的提示文案。
const GENERIC_UNSUPPORTED_MESSAGE: &str = "该客户端暂不支持自动写入配置，请复制配置手动添加";

/// stdio 接入轮廓（exe 绝对路径 + 会话 GUID）。
#[derive(Debug, Clone)]
pub struct StdioOutline {
    pub command: String,
    pub session_id: String,
}

/// 注册结果（DTO，serde camelCase 序列化给前端）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationReport {
    /// 规范化后的类型名："Codex"/"Claude"/"Cursor"/"Generic"。
    pub client_type: String,
    /// 是否支持自动写入。
    pub supported: bool,
    pub written: bool,
    pub config_path: Option<String>,
    pub backup_path: Option<String>,
    /// 预览/生成文本。
    pub config_text: String,
    /// 中文结论。
    pub message: String,
}

/// 归一化后的客户端类别（决定配置写入策略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientKind {
    Codex,
    Claude,
    Cursor,
    Generic,
}

/// 预览指定客户端的接入配置文本（不落盘）。
pub fn preview_config(
    outline: &StdioOutline,
    client_key: &str,
    home_dir: &Path,
    appdata_dir: &Path,
) -> Result<RegistrationReport, BusinessError> {
    validate_outline(outline)?;
    match resolve_client_kind(client_key) {
        ClientKind::Codex => preview_codex(outline, home_dir),
        ClientKind::Claude => preview_json_client(outline, &claude_layout(appdata_dir)),
        ClientKind::Cursor => preview_json_client(outline, &cursor_layout(home_dir)),
        ClientKind::Generic => generic_report(outline),
    }
}

/// 注册（写入）指定客户端配置：备份 → 结构化写入 → 重读验证。
/// 不支持的客户端返回 supported=false 的报告（不是 Err）。
pub fn register_config(
    outline: &StdioOutline,
    client_key: &str,
    home_dir: &Path,
    appdata_dir: &Path,
) -> Result<RegistrationReport, BusinessError> {
    validate_outline(outline)?;
    match resolve_client_kind(client_key) {
        ClientKind::Codex => register_codex(outline, home_dir),
        ClientKind::Claude => register_json_client(outline, &claude_layout(appdata_dir)),
        ClientKind::Cursor => register_json_client(outline, &cursor_layout(home_dir)),
        ClientKind::Generic => generic_report(outline),
    }
}

/// 路径健康报告（DTO，serde camelCase 序列化给前端）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathHealthReport {
    /// 规范化类型名："Codex"/"Claude"/"Cursor"/"Generic"。
    pub client_type: String,
    /// 是否支持自动写入（Generic 不参与检测）。
    pub supported: bool,
    /// 配置中已存在本产品 MCP 段（从未注册过则 false）。
    pub configured: bool,
    /// 已配置且 command 指向当前 exe 路径。
    pub healthy: bool,
    /// 配置仍使用旧版环境变量令牌形态（`env.MEMSTACK_TOKEN`），
    /// 建议改用会话 ID 接入（§7.3：env Token 兼容保留至 0.5.0 移除）。
    pub uses_legacy_env_token: bool,
    /// 配置中登记的 command 绝对路径（未配置为 None）。
    pub registered_command: Option<String>,
    pub config_path: Option<String>,
}

/// 配置段是否使用旧版环境变量令牌形态（env 可能是标准表或内联表）。
fn is_legacy_env_token_codex(server: &Table) -> bool {
    match server.get("env") {
        Some(Item::Table(table)) => table.contains_key("MEMSTACK_TOKEN"),
        Some(item) => item
            .as_value()
            .and_then(toml_edit::Value::as_inline_table)
            .is_some_and(|env| env.contains_key("MEMSTACK_TOKEN")),
        None => false,
    }
}

fn is_legacy_env_token_json(server: &serde_json::Value) -> bool {
    server
        .get("env")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|env| env.contains_key("MEMSTACK_TOKEN"))
}

/// 检查已注册客户端配置中的 command 是否仍指向当前 `MemStack-MCP.exe`
/// 路径（绿色版移动目录后失效检测，§9.4）。Generic 客户端仅预览、无落盘
/// 配置，返回 supported=false 不参与检测。
pub fn check_path_health(
    client_key: &str,
    current_exe: &str,
    home_dir: &Path,
    appdata_dir: &Path,
) -> Result<PathHealthReport, BusinessError> {
    match resolve_client_kind(client_key) {
        ClientKind::Codex => codex_path_health(current_exe, home_dir),
        ClientKind::Claude => json_path_health(current_exe, &claude_layout(appdata_dir)),
        ClientKind::Cursor => json_path_health(current_exe, &cursor_layout(home_dir)),
        ClientKind::Generic => Ok(PathHealthReport {
            client_type: "Generic".to_string(),
            supported: false,
            configured: false,
            healthy: false,
            uses_legacy_env_token: false,
            registered_command: None,
            config_path: None,
        }),
    }
}

/// Windows 路径等价比较：大小写不敏感、斜杠方向归一。
fn paths_equivalent(left: &str, right: &str) -> bool {
    let normalize = |value: &str| value.trim().replace('/', "\\").to_lowercase();
    normalize(left) == normalize(right)
}

fn codex_path_health(current_exe: &str, home_dir: &Path) -> Result<PathHealthReport, BusinessError> {
    let config_path = codex_config_path(home_dir);
    let report = |configured: bool, command: Option<String>, legacy: bool| PathHealthReport {
        client_type: "Codex".to_string(),
        supported: true,
        configured,
        healthy: command
            .as_deref()
            .is_some_and(|registered| paths_equivalent(registered, current_exe)),
        uses_legacy_env_token: legacy,
        registered_command: command,
        config_path: Some(config_path.display().to_string()),
    };
    if !config_path.is_file() {
        return Ok(report(false, None, false));
    }
    let document = read_codex_document(&config_path)?;
    let server = document
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(CODEX_SERVER_KEY))
        .and_then(Item::as_table);
    let command = server
        .and_then(|server| server.get("command"))
        .and_then(Item::as_str)
        .map(str::to_string);
    let legacy = server.is_some_and(is_legacy_env_token_codex);
    let configured = command.is_some();
    Ok(report(configured, command, legacy))
}

fn json_path_health(current_exe: &str, layout: &JsonClientLayout) -> Result<PathHealthReport, BusinessError> {
    let config_path = layout.config_path();
    let report = |configured: bool, command: Option<String>, legacy: bool| PathHealthReport {
        client_type: layout.client_type.to_string(),
        supported: true,
        configured,
        healthy: command
            .as_deref()
            .is_some_and(|registered| paths_equivalent(registered, current_exe)),
        uses_legacy_env_token: legacy,
        registered_command: command,
        config_path: Some(config_path.display().to_string()),
    };
    if !config_path.is_file() {
        return Ok(report(false, None, false));
    }
    let root = read_json_object(&config_path)?;
    let server = root.get("mcpServers").and_then(|servers| servers.get(JSON_SERVER_KEY));
    let command = server
        .and_then(|server| server.get("command"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let legacy = server.is_some_and(is_legacy_env_token_json);
    let configured = command.is_some();
    Ok(report(configured, command, legacy))
}

/// client_key 归一化映射：codex→Codex；claude/claudedesktop→Claude；cursor→Cursor；其余→Generic。
fn resolve_client_kind(client_key: &str) -> ClientKind {
    match normalize_client_key(client_key).as_str() {
        "codex" => ClientKind::Codex,
        "claude" | "claudedesktop" => ClientKind::Claude,
        "cursor" => ClientKind::Cursor,
        _ => ClientKind::Generic,
    }
}

/// 入参硬校验：exe 路径与会话 GUID 均不能为空。
fn validate_outline(outline: &StdioOutline) -> Result<(), BusinessError> {
    if outline.command.trim().is_empty() || outline.session_id.trim().is_empty() {
        return Err(BusinessError::with_message(
            ErrorCode::InvalidArgument,
            "MCP exe 路径与会话 GUID 不能为空",
        ));
    }
    Ok(())
}

/// 生成备份时间戳：本地时间 yyyyMMdd-HHmmss。
fn backup_timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// 备份已存在的配置文件为 `<文件名>.bak.<yyyyMMdd-HHmmss>`；文件不存在则不备份。
fn backup_existing(config_path: &Path) -> Result<Option<PathBuf>, BusinessError> {
    if !config_path.is_file() {
        return Ok(None);
    }
    let Some(file_name) = config_path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "配置文件路径缺少文件名，无法生成备份",
        ));
    };
    let stamp = backup_timestamp();
    let backup_path = config_path.with_file_name(format!("{file_name}.bak.{stamp}"));
    std::fs::copy(config_path, &backup_path).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("备份配置文件失败（{}）：{error}", config_path.display()),
        )
    })?;
    Ok(Some(backup_path))
}

// ---- Codex（~/.codex/config.toml，toml_edit 结构化编辑）----

/// Codex 配置文件路径：`~/.codex/config.toml`。
fn codex_config_path(home_dir: &Path) -> PathBuf {
    home_dir.join(".codex").join("config.toml")
}

/// 读取 Codex config.toml；文件不存在则返回空文档；存在但解析失败即 Err。
fn read_codex_document(config_path: &Path) -> Result<DocumentMut, BusinessError> {
    if !config_path.is_file() {
        return Ok(DocumentMut::new());
    }
    let text = std::fs::read_to_string(config_path).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("读取 Codex config.toml 失败（{}）：{error}", config_path.display()),
        )
    })?;
    text.parse::<DocumentMut>().map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!(
                "Codex config.toml 不是合法 TOML，请先修复或删除该文件（{}）：{error}",
                config_path.display()
            ),
        )
    })
}

/// 按总计划 §9.2 构造 `[mcp_servers.memstack]` 段（command/args/enabled/required/两个超时）。
fn codex_server_table(outline: &StdioOutline) -> Table {
    let mut table = Table::new();
    table["command"] = value(outline.command.clone());
    let mut arguments = Array::new();
    arguments.push(SESSION_ID_ARGUMENT);
    arguments.push(outline.session_id.clone());
    table["args"] = value(arguments);
    table["enabled"] = value(true);
    table["required"] = value(false);
    table["startup_timeout_sec"] = value(10);
    table["tool_timeout_sec"] = value(60);
    table
}

/// 结构化合并：写入/覆盖 memstack 段，不触碰其他段落与注释。
fn upsert_codex_server(document: &mut DocumentMut, outline: &StdioOutline) -> Result<(), BusinessError> {
    let root = document.as_table_mut();
    if !root.contains_key("mcp_servers") {
        // 隐式父表：渲染时只输出 [mcp_servers.memstack] 头，避免多余的 [mcp_servers] 空段。
        let mut servers = Table::new();
        servers.set_implicit(true);
        root.insert("mcp_servers", Item::Table(servers));
    }
    let Some(servers) = root.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "Codex config.toml 中的 mcp_servers 不是表结构，无法写入 MCP 配置",
        ));
    };
    servers.insert(CODEX_SERVER_KEY, Item::Table(codex_server_table(outline)));
    Ok(())
}

/// 生成合并后的完整 TOML 文本（不落盘）。
fn codex_merged_text(config_path: &Path, outline: &StdioOutline) -> Result<String, BusinessError> {
    let mut document = read_codex_document(config_path)?;
    upsert_codex_server(&mut document, outline)?;
    Ok(document.to_string())
}

/// 重读验证：exe 路径与会话 GUID 必须与写入值一致。
fn verify_codex(config_path: &Path, outline: &StdioOutline) -> Result<(), BusinessError> {
    let document = read_codex_document(config_path)?;
    let Some(server) = document
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get(CODEX_SERVER_KEY))
        .and_then(Item::as_table)
    else {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "Codex config.toml 重读验证失败：未找到 mcp_servers.memstack 段",
        ));
    };
    let command_ok = server.get("command").and_then(Item::as_str) == Some(outline.command.as_str());
    let arguments_ok = server.get("args").and_then(Item::as_array).is_some_and(|arguments| {
        arguments.len() == 2
            && arguments.get(0).and_then(toml_edit::Value::as_str) == Some(SESSION_ID_ARGUMENT)
            && arguments.get(1).and_then(toml_edit::Value::as_str) == Some(outline.session_id.as_str())
    });
    if command_ok && arguments_ok {
        Ok(())
    } else {
        Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "Codex config.toml 重读验证失败：exe 路径或会话 GUID 与写入值不一致",
        ))
    }
}

/// Codex 预览：仅给出 `[mcp_servers.memstack]` 段文本（用户验收反馈：
/// 不要把用户整个 Codex 配置搬进预览；完整合并文本只在写入时落盘）。
fn preview_codex(outline: &StdioOutline, home_dir: &Path) -> Result<RegistrationReport, BusinessError> {
    let config_path = codex_config_path(home_dir);
    Ok(RegistrationReport {
        client_type: "Codex".to_string(),
        supported: true,
        written: false,
        config_path: Some(config_path.display().to_string()),
        backup_path: None,
        config_text: codex_snippet_text(outline),
        message: "Codex 接入配置预览（未写入）".to_string(),
    })
}

/// Codex 注册：目录不存在则创建 `~/.codex`；文件已存在则先备份再结构化写入，最后重读验证。
/// 报告中的 config_text 仅含 MCP 段（与预览一致）；完整合并文本写入磁盘。
fn register_codex(outline: &StdioOutline, home_dir: &Path) -> Result<RegistrationReport, BusinessError> {
    let config_path = codex_config_path(home_dir);
    let config_text = codex_merged_text(&config_path, outline)?;
    let backup_path = backup_existing(&config_path)?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("创建 Codex 配置目录失败：{error}"))
        })?;
    }
    std::fs::write(&config_path, &config_text).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("写入 Codex config.toml 失败（{}）：{error}", config_path.display()),
        )
    })?;
    verify_codex(&config_path, outline)?;
    Ok(RegistrationReport {
        client_type: "Codex".to_string(),
        supported: true,
        written: true,
        config_path: Some(config_path.display().to_string()),
        backup_path: backup_path.map(|path| path.display().to_string()),
        config_text: codex_snippet_text(outline),
        message: "已写入 Codex MCP 配置并完成重读验证".to_string(),
    })
}

/// 生成仅含 MCP 段的 TOML 文本：`[mcp_servers.memstack]` 起头的独立片段。
fn codex_snippet_text(outline: &StdioOutline) -> String {
    let mut document = DocumentMut::new();
    let mut servers = toml_edit::Table::new();
    servers.set_implicit(true);
    servers.insert(CODEX_SERVER_KEY, Item::Table(codex_server_table(outline)));
    document.as_table_mut().insert("mcp_servers", Item::Table(servers));
    document.to_string()
}

// ---- Claude Desktop / Cursor（JSON 合并写入）----

/// JSON 型客户端的文件布局与中文文案。
struct JsonClientLayout {
    /// 规范化类型名。
    client_type: &'static str,
    /// 配置所在目录（Claude：`%APPDATA%\Claude`；Cursor：`%USERPROFILE%\.cursor`）。
    config_dir: PathBuf,
    /// 配置文件名。
    config_file: &'static str,
    /// 未检测到客户端时的中文提示。
    missing_message: &'static str,
    /// 写入成功后的中文结论。
    success_message: &'static str,
}

impl JsonClientLayout {
    /// 配置文件完整路径。
    fn config_path(&self) -> PathBuf {
        self.config_dir.join(self.config_file)
    }

    /// detect_installed 语义并入 supported：目录或配置文件存在即视为已安装。
    fn installed(&self) -> bool {
        self.config_dir.is_dir() || self.config_path().is_file()
    }
}

/// Claude Desktop 布局：`%APPDATA%\Claude\claude_desktop_config.json`。
fn claude_layout(appdata_dir: &Path) -> JsonClientLayout {
    JsonClientLayout {
        client_type: "Claude",
        config_dir: appdata_dir.join("Claude"),
        config_file: "claude_desktop_config.json",
        missing_message: "未检测到 Claude Desktop，请先安装或手动复制配置",
        success_message: "已写入 Claude Desktop MCP 配置并完成重读验证",
    }
}

/// Cursor 布局：`%USERPROFILE%\.cursor\mcp.json`。
fn cursor_layout(home_dir: &Path) -> JsonClientLayout {
    JsonClientLayout {
        client_type: "Cursor",
        config_dir: home_dir.join(".cursor"),
        config_file: "mcp.json",
        missing_message: "未检测到 Cursor，请先安装或手动复制配置",
        success_message: "已写入 Cursor MCP 配置并完成重读验证",
    }
}

/// 读取现有 JSON 配置为顶层对象；文件不存在则空对象起步；存在但非法即 Err。
fn read_json_object(config_path: &Path) -> Result<serde_json::Map<String, serde_json::Value>, BusinessError> {
    if !config_path.is_file() {
        return Ok(serde_json::Map::new());
    }
    let text = std::fs::read_to_string(config_path).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("读取配置文件失败（{}）：{error}", config_path.display()),
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!(
                "配置文件不是合法 JSON，请先修复或删除该文件（{}）：{error}",
                config_path.display()
            ),
        )
    })?;
    match value {
        serde_json::Value::Object(map) => Ok(map),
        _ => Err(BusinessError::with_message(
            ErrorCode::InternalError,
            format!("配置文件顶层不是 JSON 对象：{}", config_path.display()),
        )),
    }
}

/// memstack 服务对象（command + args）。
fn memstack_server_value(outline: &StdioOutline) -> serde_json::Value {
    serde_json::json!({
        "command": outline.command.clone(),
        "args": [SESSION_ID_ARGUMENT, outline.session_id.clone()],
    })
}

/// 合并后的完整 JSON 文本（两空格缩进，保留未知键；不落盘）。
fn merged_json_text(config_path: &Path, outline: &StdioOutline) -> Result<String, BusinessError> {
    let mut root = read_json_object(config_path)?;
    let servers_entry = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(servers) = servers_entry.as_object_mut() else {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "配置文件中的 mcpServers 不是对象，无法合并 MCP 配置",
        ));
    };
    servers.insert(JSON_SERVER_KEY.to_string(), memstack_server_value(outline));
    serde_json::to_string_pretty(&serde_json::Value::Object(root)).map_err(|error| {
        BusinessError::with_message(ErrorCode::InternalError, format!("序列化配置 JSON 失败：{error}"))
    })
}

/// 重读验证：exe 路径与会话 GUID 必须与写入值一致。
fn verify_json(config_path: &Path, outline: &StdioOutline) -> Result<(), BusinessError> {
    let text = std::fs::read_to_string(config_path).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("重读配置文件失败（{}）：{error}", config_path.display()),
        )
    })?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("重读配置文件不是合法 JSON（{}）：{error}", config_path.display()),
        )
    })?;
    let Some(server) = value.get("mcpServers").and_then(|servers| servers.get(JSON_SERVER_KEY)) else {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "配置文件重读验证失败：未找到 mcpServers.memstack",
        ));
    };
    let command_ok = server.get("command").and_then(serde_json::Value::as_str) == Some(outline.command.as_str());
    let arguments_ok = server
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|arguments| {
            arguments.len() == 2
                && arguments[0].as_str() == Some(SESSION_ID_ARGUMENT)
                && arguments[1].as_str() == Some(outline.session_id.as_str())
        });
    if command_ok && arguments_ok {
        Ok(())
    } else {
        Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "配置文件重读验证失败：exe 路径或会话 GUID 与写入值不一致",
        ))
    }
}

/// JSON 型客户端预览：仅给出 mcpServers 段片段（与 Codex 口径一致，不搬用户完整配置）。
fn preview_json_client(outline: &StdioOutline, layout: &JsonClientLayout) -> Result<RegistrationReport, BusinessError> {
    let config_path = layout.config_path();
    let supported = layout.installed();
    let message = if supported {
        format!("{} 接入配置预览（未写入）", layout.client_type)
    } else {
        layout.missing_message.to_string()
    };
    Ok(RegistrationReport {
        client_type: layout.client_type.to_string(),
        supported,
        written: false,
        config_path: Some(config_path.display().to_string()),
        backup_path: None,
        config_text: json_snippet_text(outline)?,
        message,
    })
}

/// JSON 型客户端注册：未检测到客户端时不落盘；否则备份 → 合并写入 → 重读验证。
fn register_json_client(
    outline: &StdioOutline,
    layout: &JsonClientLayout,
) -> Result<RegistrationReport, BusinessError> {
    let config_path = layout.config_path();
    if !layout.installed() {
        // 未检测到客户端：不落盘、不伪装已连接，仅给出可复制文本。
        return Ok(RegistrationReport {
            client_type: layout.client_type.to_string(),
            supported: false,
            written: false,
            config_path: Some(config_path.display().to_string()),
            backup_path: None,
            config_text: json_snippet_text(outline)?,
            message: layout.missing_message.to_string(),
        });
    }
    let config_text = merged_json_text(&config_path, outline)?;
    let backup_path = backup_existing(&config_path)?;
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            BusinessError::with_message(
                ErrorCode::InternalError,
                format!("创建配置目录失败（{}）：{error}", parent.display()),
            )
        })?;
    }
    std::fs::write(&config_path, &config_text).map_err(|error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("写入配置文件失败（{}）：{error}", config_path.display()),
        )
    })?;
    verify_json(&config_path, outline)?;
    Ok(RegistrationReport {
        client_type: layout.client_type.to_string(),
        supported: true,
        written: true,
        config_path: Some(config_path.display().to_string()),
        backup_path: backup_path.map(|path| path.display().to_string()),
        config_text: json_snippet_text(outline)?,
        message: layout.success_message.to_string(),
    })
}

// ---- 通用客户端（Trae / WorkBuddy / Qoder / 未知）----

/// 通用客户端报告：仅生成可复制的 stdio 配置 JSON（两空格缩进），不落盘。
fn generic_report(outline: &StdioOutline) -> Result<RegistrationReport, BusinessError> {
    Ok(RegistrationReport {
        client_type: "Generic".to_string(),
        supported: false,
        written: false,
        config_path: None,
        backup_path: None,
        config_text: json_snippet_text(outline)?,
        message: GENERIC_UNSUPPORTED_MESSAGE.to_string(),
    })
}

/// 生成仅含 mcpServers 段的 JSON 片段文本（两空格缩进），供预览/复制，不含用户其余配置。
fn json_snippet_text(outline: &StdioOutline) -> Result<String, BusinessError> {
    let mut servers = serde_json::Map::new();
    servers.insert(JSON_SERVER_KEY.to_string(), memstack_server_value(outline));
    let mut root = serde_json::Map::new();
    root.insert("mcpServers".to_string(), serde_json::Value::Object(servers));
    serde_json::to_string_pretty(&serde_json::Value::Object(root)).map_err(|error| {
        BusinessError::with_message(ErrorCode::InternalError, format!("序列化 MCP 配置 JSON 失败：{error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    /// 构造带反斜杠路径的接入轮廓（贴近 Windows 真实形态）。
    fn outline() -> StdioOutline {
        StdioOutline {
            command: r"C:\Users\z\AppData\Local\Programs\MemStack\MemStack-MCP.exe".to_string(),
            session_id: "0f3d9a1e-1111-4222-8333-444455556666".to_string(),
        }
    }

    #[test]
    fn codex_register_preserves_existing_sections_and_comments() {
        let home = TempDir::new().unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        let config_path = codex_dir.join("config.toml");
        let original = "# 顶部注释\n\n[model]\nprovider = \"x\"\n\n# 既有 MCP 注释\n[mcp_servers.other]\ncommand = \"other.exe\"\n";
        fs::write(&config_path, original).unwrap();

        let report = register_config(&outline(), "codex", home.path(), home.path()).unwrap();
        assert!(report.supported);
        assert!(report.written);
        assert_eq!(report.client_type, "Codex");

        // 备份文件存在，且内容与原文件一致。
        let backup_path = report.backup_path.as_ref().unwrap();
        assert!(Path::new(backup_path).is_file());
        assert_eq!(fs::read_to_string(backup_path).unwrap(), original);

        // 注释与既有段落保留（toml_edit 特性）。
        let text = fs::read_to_string(&config_path).unwrap();
        assert!(text.contains("# 顶部注释"));
        assert!(text.contains("# 既有 MCP 注释"));
        assert!(text.contains("provider = \"x\""));
        assert!(text.contains("[mcp_servers.other]"));

        // 新段结构与总计划 §9.2 一致（含重读验证语义：注册返回 Ok 即已通过验证）。
        let document: DocumentMut = text.parse().unwrap();
        let server = document
            .get("mcp_servers")
            .and_then(Item::as_table)
            .and_then(|servers| servers.get(CODEX_SERVER_KEY))
            .and_then(Item::as_table)
            .unwrap();
        assert_eq!(
            server.get("command").and_then(Item::as_str),
            Some(outline().command.as_str())
        );
        assert_eq!(server.get("enabled").and_then(Item::as_bool), Some(true));
        assert_eq!(server.get("required").and_then(Item::as_bool), Some(false));
        assert_eq!(server.get("startup_timeout_sec").and_then(Item::as_integer), Some(10));
        assert_eq!(server.get("tool_timeout_sec").and_then(Item::as_integer), Some(60));
        let arguments = server.get("args").and_then(Item::as_array).unwrap();
        assert_eq!(
            arguments.get(0).and_then(toml_edit::Value::as_str),
            Some(SESSION_ID_ARGUMENT)
        );
        assert_eq!(
            arguments.get(1).and_then(toml_edit::Value::as_str),
            Some(outline().session_id.as_str())
        );
    }

    #[test]
    fn codex_register_creates_missing_config_without_backup() {
        let home = TempDir::new().unwrap();
        let report = register_config(&outline(), "codex", home.path(), home.path()).unwrap();
        assert!(report.supported);
        assert!(report.written);
        assert!(report.backup_path.is_none());
        let config_path = home.path().join(".codex").join("config.toml");
        assert!(config_path.is_file());
        let text = fs::read_to_string(&config_path).unwrap();
        // 新文件仅含该段：首行即 [mcp_servers.memstack]（父表隐式）。
        assert!(text.starts_with("[mcp_servers.memstack]"));
        assert!(!text.contains("[mcp_servers.other]"));

        // 预览（不落盘）与首次写入文本一致。
        let preview = preview_config(&outline(), "codex", home.path(), home.path()).unwrap();
        assert!(!preview.written);
        assert_eq!(preview.config_text, text);
    }

    #[test]
    fn codex_preview_and_report_show_only_mcp_section() {
        let home = TempDir::new().unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            "# 我的顶部注释\n[model]\nprovider = \"x\"\n",
        )
        .unwrap();

        let preview = preview_config(&outline(), "codex", home.path(), home.path()).unwrap();
        assert!(preview.config_text.starts_with("[mcp_servers.memstack]"));
        assert!(!preview.config_text.contains("provider"), "预览不应搬出用户既有配置");
        assert!(!preview.config_text.contains("我的顶部注释"));

        // 注册报告同样只回段文本；磁盘上的完整文件保留用户段落。
        let report = register_config(&outline(), "codex", home.path(), home.path()).unwrap();
        assert!(report.config_text.starts_with("[mcp_servers.memstack]"));
        assert!(!report.config_text.contains("provider"));
        let on_disk = fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(on_disk.contains("provider = \"x\""), "落盘文件保留用户段落");
    }

    #[test]
    fn codex_register_rejects_invalid_toml() {
        let home = TempDir::new().unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(codex_dir.join("config.toml"), "not [ valid toml").unwrap();
        let result = register_config(&outline(), "codex", home.path(), home.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, ErrorCode::InternalError);
    }

    #[test]
    fn claude_register_preserves_unknown_keys() {
        let home = TempDir::new().unwrap();
        let claude_dir = home.path().join("Claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let config_path = claude_dir.join("claude_desktop_config.json");
        fs::write(
            &config_path,
            r#"{ "globalShortcut": "Ctrl+Alt+M", "mcpServers": { "existing": { "command": "x.exe" } } }"#,
        )
        .unwrap();

        let report = register_config(&outline(), "Claude Desktop", home.path(), home.path()).unwrap();
        assert!(report.supported);
        assert!(report.written);
        assert_eq!(report.client_type, "Claude");
        assert!(Path::new(report.backup_path.as_ref().unwrap()).is_file());

        // 未知键与既有 MCP 服务保留，memstack 正确写入。
        let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(value["globalShortcut"], "Ctrl+Alt+M");
        assert_eq!(value["mcpServers"]["existing"]["command"], "x.exe");
        assert_eq!(value["mcpServers"][JSON_SERVER_KEY]["command"], outline().command);
        assert_eq!(value["mcpServers"][JSON_SERVER_KEY]["args"][0], SESSION_ID_ARGUMENT);
        assert_eq!(value["mcpServers"][JSON_SERVER_KEY]["args"][1], outline().session_id);

        // 报告只回 mcpServers 段片段，不搬用户其余配置。
        let snippet: serde_json::Value = serde_json::from_str(&report.config_text).unwrap();
        assert!(snippet.get("mcpServers").is_some());
        assert!(snippet.get("globalShortcut").is_none(), "报告不应包含用户其余配置");
    }

    #[test]
    fn cursor_register_is_idempotent_with_backup_each_time() {
        let home = TempDir::new().unwrap();
        // 客户端已安装（目录存在）但尚无 mcp.json。
        fs::create_dir_all(home.path().join(".cursor")).unwrap();

        let first = register_config(&outline(), "cursor", home.path(), home.path()).unwrap();
        assert!(first.supported);
        assert!(first.written);
        assert!(first.backup_path.is_none());
        let config_path = home.path().join(".cursor").join("mcp.json");
        assert!(config_path.is_file());
        let first_text = fs::read_to_string(&config_path).unwrap();

        // 再次注册（幂等覆盖同一会话）：段内容一致、第二次也有备份。
        let second = register_config(&outline(), "cursor", home.path(), home.path()).unwrap();
        assert!(second.written);
        assert!(Path::new(second.backup_path.as_ref().unwrap()).is_file());
        assert_eq!(fs::read_to_string(&config_path).unwrap(), first_text);
    }

    #[test]
    fn generic_clients_are_preview_only_without_writes() {
        for client_key in ["trae", "workbuddy", "qoder", "totally-unknown"] {
            let home = TempDir::new().unwrap();
            let report = register_config(&outline(), client_key, home.path(), home.path()).unwrap();
            assert!(!report.supported, "client_key={client_key}");
            assert!(!report.written, "client_key={client_key}");
            assert_eq!(report.client_type, "Generic");
            assert!(report.config_path.is_none());
            assert!(report.backup_path.is_none());
            assert_eq!(report.message, GENERIC_UNSUPPORTED_MESSAGE);

            // config_text 为合法 JSON，且含 exe 路径与 --session-id。
            let value: serde_json::Value = serde_json::from_str(&report.config_text).unwrap();
            assert_eq!(value["mcpServers"][JSON_SERVER_KEY]["command"], outline().command);
            assert_eq!(value["mcpServers"][JSON_SERVER_KEY]["args"][0], SESSION_ID_ARGUMENT);
            assert_eq!(value["mcpServers"][JSON_SERVER_KEY]["args"][1], outline().session_id);

            // 不落盘：目录内未产生任何文件。
            assert!(fs::read_dir(home.path()).unwrap().next().is_none());
        }
    }

    #[test]
    fn client_key_normalization_maps_supported_clients() {
        let home = TempDir::new().unwrap();
        assert_eq!(
            preview_config(&outline(), "  CoDex ", home.path(), home.path())
                .unwrap()
                .client_type,
            "Codex"
        );
        assert_eq!(
            preview_config(&outline(), "Claude Desktop", home.path(), home.path())
                .unwrap()
                .client_type,
            "Claude"
        );
        assert_eq!(
            preview_config(&outline(), "claudedesktop", home.path(), home.path())
                .unwrap()
                .client_type,
            "Claude"
        );
        assert_eq!(
            preview_config(&outline(), "Cursor", home.path(), home.path())
                .unwrap()
                .client_type,
            "Cursor"
        );
        let generic = preview_config(&outline(), "workbuddy", home.path(), home.path()).unwrap();
        assert_eq!(generic.client_type, "Generic");
        assert!(!generic.supported);
    }

    #[test]
    fn undetected_json_clients_report_missing_message_without_writes() {
        let home = TempDir::new().unwrap();
        let claude = register_config(&outline(), "claude", home.path(), home.path()).unwrap();
        assert!(!claude.supported);
        assert!(!claude.written);
        assert!(claude.message.contains("未检测到 Claude Desktop"));
        assert!(!home.path().join("Claude").exists());

        let cursor = register_config(&outline(), "cursor", home.path(), home.path()).unwrap();
        assert!(!cursor.supported);
        assert!(!cursor.written);
        assert!(cursor.message.contains("未检测到 Cursor"));
        assert!(!home.path().join(".cursor").exists());
    }

    #[test]
    fn empty_outline_is_rejected() {
        let home = TempDir::new().unwrap();
        let result = register_config(
            &StdioOutline {
                command: "  ".to_string(),
                session_id: String::new(),
            },
            "codex",
            home.path(),
            home.path(),
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn path_health_matches_when_command_points_to_current_exe() {
        let home = TempDir::new().unwrap();
        let outline = outline();
        register_config(&outline, "codex", home.path(), home.path()).unwrap();

        // 同路径（含大小写与斜杠差异）→ healthy。
        let health = check_path_health("codex", &outline.command, home.path(), home.path()).unwrap();
        assert!(health.supported);
        assert!(health.configured);
        assert!(health.healthy);
        assert_eq!(health.registered_command.as_deref(), Some(outline.command.as_str()));

        // 大小写不敏感等价。
        let upper = outline.command.to_uppercase();
        let health = check_path_health("codex", &upper, home.path(), home.path()).unwrap();
        assert!(health.healthy, "Windows 路径大小写不敏感");
    }

    #[test]
    fn path_health_reports_stale_after_directory_move() {
        let home = TempDir::new().unwrap();
        let outline = outline();
        register_config(&outline, "codex", home.path(), home.path()).unwrap();

        // 绿色版移动后：当前 exe 在新位置，配置仍指向旧路径 → 不健康。
        let moved = r"D:\NewLocation\MemStack\MemStack-MCP.exe".to_string();
        let health = check_path_health("codex", &moved, home.path(), home.path()).unwrap();
        assert!(health.configured, "配置段仍存在");
        assert!(!health.healthy, "路径不一致应报告失效");
        assert_eq!(health.registered_command.as_deref(), Some(outline.command.as_str()));
    }

    #[test]
    fn path_health_unconfigured_when_section_missing() {
        let home = TempDir::new().unwrap();
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(codex_dir.join("config.toml"), "[model]\nprovider = \"x\"\n").unwrap();

        let health = check_path_health("codex", r"C:\any\MemStack-MCP.exe", home.path(), home.path()).unwrap();
        assert!(!health.configured);
        assert!(!health.healthy);
        assert!(health.registered_command.is_none());
        assert!(health.config_path.is_some());

        // 文件整体不存在同样视为未配置。
        let claude = check_path_health("claude", r"C:\any\MemStack-MCP.exe", home.path(), home.path()).unwrap();
        assert!(!claude.configured);
        assert!(claude.supported);
    }

    #[test]
    fn path_health_json_clients_and_generic() {
        let home = TempDir::new().unwrap();
        fs::create_dir_all(home.path().join(".cursor")).unwrap();
        let outline = outline();
        register_config(&outline, "cursor", home.path(), home.path()).unwrap();

        let health = check_path_health("cursor", &outline.command, home.path(), home.path()).unwrap();
        assert_eq!(health.client_type, "Cursor");
        assert!(health.healthy);

        let moved = r"E:\elsewhere\MemStack-MCP.exe".to_string();
        let health = check_path_health("cursor", &moved, home.path(), home.path()).unwrap();
        assert!(!health.healthy);

        // Generic：不参与检测。
        let generic = check_path_health("workbuddy", &outline.command, home.path(), home.path()).unwrap();
        assert!(!generic.supported);
        assert!(!generic.configured);
    }

    #[test]
    fn reregister_after_move_restores_health() {
        let home = TempDir::new().unwrap();
        let outline = outline();
        register_config(&outline, "codex", home.path(), home.path()).unwrap();

        // 模拟绿色版移动：新位置重新注册（一键重新注册路径），session 不变。
        let moved_outline = StdioOutline {
            command: r"E:\Moved\MemStack\MemStack-MCP.exe".to_string(),
            session_id: outline.session_id.clone(),
        };
        let report = register_config(&moved_outline, "codex", home.path(), home.path()).unwrap();
        assert!(report.written, "重注册应再次写入并备份");

        let health = check_path_health("codex", &moved_outline.command, home.path(), home.path()).unwrap();
        assert!(health.healthy, "重注册后应恢复健康");
        assert!(!health.uses_legacy_env_token, "新写入形态为会话 ID");
    }

    #[test]
    fn legacy_env_token_config_is_detected_and_upgradeable() {
        let home = TempDir::new().unwrap();
        // 旧版写入的 env Token 形态（Codex TOML）。
        let codex_dir = home.path().join(".codex");
        fs::create_dir_all(&codex_dir).unwrap();
        fs::write(
            codex_dir.join("config.toml"),
            format!(
                "[mcp_servers.memstack]\ncommand = '{}'\nenv = {{ MEMSTACK_TOKEN = \"legacy-secret\" }}\n",
                outline().command
            ),
        )
        .unwrap();
        let health = check_path_health("codex", &outline().command, home.path(), home.path()).unwrap();
        assert!(health.healthy, "路径本身一致");
        assert!(health.uses_legacy_env_token, "env Token 形态应被识别");

        // JSON 客户端同样识别。
        let claude_dir = home.path().join("Claude");
        fs::create_dir_all(&claude_dir).unwrap();
        fs::write(
            claude_dir.join("claude_desktop_config.json"),
            format!(
                r#"{{"mcpServers":{{"memstack":{{"command":"{}","env":{{"MEMSTACK_TOKEN":"x"}}}}}}}}"#,
                outline().command.replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let health = check_path_health("claude", &outline().command, home.path(), home.path()).unwrap();
        assert!(health.uses_legacy_env_token);

        // 重新注册（写入会话 ID 形态）后 legacy 标记消除。
        let outline = outline();
        register_config(&outline, "codex", home.path(), home.path()).unwrap();
        let health = check_path_health("codex", &outline.command, home.path(), home.path()).unwrap();
        assert!(!health.uses_legacy_env_token, "重注册后应消除 env Token 形态");
        assert!(health.healthy);
    }

    #[test]
    fn generated_config_is_session_id_form_only() {
        // §7.3：注册/预览只产出 --session-id 形态，不再生成 env Token 形态。
        let home = TempDir::new().unwrap();
        let outline = outline();
        for client_key in ["codex", "claude", "cursor", "workbuddy"] {
            let report = preview_config(&outline, client_key, home.path(), home.path()).unwrap();
            assert!(
                !report.config_text.contains("MEMSTACK_TOKEN"),
                "{client_key} 配置文本不得包含 env Token"
            );
            assert!(
                report.config_text.contains(SESSION_ID_ARGUMENT),
                "{client_key} 配置文本必须含 --session-id"
            );
        }
    }
}
