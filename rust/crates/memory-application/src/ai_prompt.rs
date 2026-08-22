//! AI 全局提示词：生成、客户端适配、预览与安装（执行计划 §20、§21.5）。
//!
//! - 提示词只保存稳定工作流；文档模板、字段枚举、校验规则由 MCP 工具描述提供。
//! - 通用提示词用 `memstack.工具名` 表达逻辑归属；客户端适配器将逻辑名转换成
//!   该客户端实际识别的工具名称（MCP 服务名、逻辑工具名与实际工具名分别建模）。
//! - 安装只写入带标记的专属片段：写入前备份、写入后验证、重复安装幂等，
//!   保留用户文件中与 MemStack 无关的内容；不支持自动安装的客户端仅提供查看与复制。
//! - 提示词不包含访问令牌、数据库地址或用户私有路径。

use std::path::{Path, PathBuf};

use memory_domain::{BusinessError, ErrorCode};

/// 提示词模板版本（内容变化时递增，供界面展示「待更新」状态）。
pub const PROMPT_TEMPLATE_VERSION: &str = "2";

/// MCP 服务标识（§9.1：AI 必须先锁定该服务再选工具）。
pub const MCP_SERVICE_ID: &str = "memstack";

/// 片段标记（幂等安装的边界）。
const SECTION_BEGIN: &str = "<!-- MEMSTACK:PROMPT:BEGIN -->";
const SECTION_END: &str = "<!-- MEMSTACK:PROMPT:END -->";

/// 提示词报告（DTO，serde camelCase 序列化给前端）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptReport {
    /// 规范化后的客户端类型："Codex"/"Claude"/"Cursor"/"Generic"。
    pub client_type: String,
    /// 提示词模板版本。
    pub template_version: String,
    /// MCP 服务标识。
    pub service_id: String,
    /// 完整、只读的最终提示词（复制与安装使用同一生成结果）。
    pub prompt_text: String,
    /// 是否支持自动安装。
    pub can_install: bool,
    /// 安装目标文件（能安全识别时提供）。
    pub config_path: Option<String>,
    /// 目标文件是否已包含当前版本的 MemStack 片段。
    pub installed: bool,
    /// 当前配置与目标配置的差异说明（未安装 / 已是最新 / 需要更新）。
    pub diff_text: String,
    /// 安装动作附带的备份路径（仅安装结果携带）。
    pub backup_path: Option<String>,
}

/// 按客户端类型生成提示词预览（不落盘）。
pub fn preview_prompt(client_key: &str, home_dir: &Path, appdata_dir: &Path) -> Result<PromptReport, BusinessError> {
    let kind = resolve_client_kind(client_key);
    let prompt_text = render_prompt(kind);
    let (can_install, config_path) = install_target(kind, home_dir, appdata_dir);
    let installed = config_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|content| contains_current_section(&content, &prompt_text));
    let diff_text = if !can_install {
        "该客户端暂不支持自动安装，请复制提示词手动配置".to_string()
    } else if installed {
        "目标配置已包含当前版本的 MemStack 提示词".to_string()
    } else if config_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|content| content.contains(SECTION_BEGIN))
    {
        "目标配置包含旧版 MemStack 提示词，安装后将更新为当前版本".to_string()
    } else {
        "目标配置尚未包含 MemStack 提示词，安装后将追加专属片段".to_string()
    };
    Ok(PromptReport {
        client_type: kind.display_name().to_string(),
        template_version: PROMPT_TEMPLATE_VERSION.to_string(),
        service_id: MCP_SERVICE_ID.to_string(),
        prompt_text,
        can_install,
        config_path,
        installed,
        diff_text,
        backup_path: None,
    })
}

/// 安装提示词到目标客户端配置（用户点击安装后调用）。
///
/// 流程：读取现有内容 → 备份 → 替换/追加标记片段 → 写回 → 重读验证。
pub fn install_prompt(client_key: &str, home_dir: &Path, appdata_dir: &Path) -> Result<PromptReport, BusinessError> {
    let kind = resolve_client_kind(client_key);
    let prompt_text = render_prompt(kind);
    let (can_install, config_path) = install_target(kind, home_dir, appdata_dir);
    if !can_install {
        // 不支持自动安装：返回可复制报告，不伪装写入。
        return preview_prompt(client_key, home_dir, appdata_dir);
    }
    let path = PathBuf::from(config_path.clone().unwrap_or_default());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| io_error("创建配置目录失败", &path, error))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge_section(&existing, &prompt_text);
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let backup_path = format!("{}.bak.{}", path.display(), timestamp);
    if !existing.trim().is_empty() {
        std::fs::write(&backup_path, &existing).map_err(|error| io_error("写入备份失败", &path, error))?;
    }
    std::fs::write(&path, &merged).map_err(|error| io_error("写入提示词失败", &path, error))?;
    // 写入后验证：重读并确认片段存在。
    let verified = std::fs::read_to_string(&path)
        .map(|content| contains_current_section(&content, &prompt_text))
        .unwrap_or(false);
    if !verified {
        // 恢复备份，避免留下半成品。
        if !existing.trim().is_empty() {
            let _ = std::fs::write(&path, &existing);
        }
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "提示词写入后验证失败，已恢复原配置",
        ));
    }
    let installed = true;
    Ok(PromptReport {
        client_type: kind.display_name().to_string(),
        template_version: PROMPT_TEMPLATE_VERSION.to_string(),
        service_id: MCP_SERVICE_ID.to_string(),
        prompt_text,
        can_install,
        config_path,
        installed,
        diff_text: "安装完成：目标配置已包含当前版本的 MemStack 提示词".to_string(),
        backup_path: (!existing.trim().is_empty()).then_some(backup_path),
    })
}

/// 客户端类别（决定工具名转换与安装目标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientKind {
    Codex,
    Claude,
    Cursor,
    Generic,
}

impl ClientKind {
    fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Cursor => "Cursor",
            Self::Generic => "Generic",
        }
    }

    /// 逻辑工具名 → 该客户端实际识别的工具名（§9.1 适配器）。
    fn actual_tool_name(self, logical: &str) -> String {
        match self {
            Self::Codex => format!("mcp__{MCP_SERVICE_ID}__{logical}"),
            Self::Claude => format!("mcp__{MCP_SERVICE_ID}__{logical}"),
            Self::Cursor => format!("{MCP_SERVICE_ID}_{logical}"),
            Self::Generic => format!("{MCP_SERVICE_ID}.{logical}"),
        }
    }
}

fn resolve_client_kind(client_key: &str) -> ClientKind {
    let normalized = client_key.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "codex" => ClientKind::Codex,
        "claude" | "claude desktop" | "claude-desktop" => ClientKind::Claude,
        "cursor" => ClientKind::Cursor,
        _ => ClientKind::Generic,
    }
}

/// 安装目标（§20.2：不同客户端的提示词文件路径隔离，禁止模式参数分支混用）。
fn install_target(kind: ClientKind, home_dir: &Path, appdata_dir: &Path) -> (bool, Option<String>) {
    match kind {
        ClientKind::Codex => (
            true,
            Some(home_dir.join(".codex").join("AGENTS.md").to_string_lossy().into_owned()),
        ),
        ClientKind::Cursor => (
            true,
            Some(
                home_dir
                    .join(".cursor")
                    .join("rules")
                    .join("memstack.mdc")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ),
        ClientKind::Claude => (
            // Claude Desktop 无稳定全局提示词文件：仅查看与复制。
            false,
            Some(
                appdata_dir
                    .join("Claude")
                    .join("claude_desktop_config.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ),
        ClientKind::Generic => (false, None),
    }
}

/// 渲染最终提示词（按客户端转换工具名；同一结果用于复制与安装）。
fn render_prompt(kind: ClientKind) -> String {
    let tool = |logical: &str| kind.actual_tool_name(logical);
    format!(
        "# MemStack 项目记忆工作流（服务标识：{MCP_SERVICE_ID}）\n\n\
         1. 仅当对话存在明确的本地工作空间时启用项目记忆；纯聊天、普通问答不初始化项目文档。\n\
         2. 项目记忆操作只能使用 MCP 服务标识为 `{MCP_SERVICE_ID}` 的工具；禁止使用其他 MCP 服务中名称或用途相近的工具代替。\n\
         3. 每个有本地工作空间的新会话开始时，调用一次 `{handoff}`（workspacePath=工作空间绝对路径，全部预算参数显式传递，不使用默认值）。\n\
         4. 返回 `NOT_INITIALIZED` 时：检查仓库的项目说明、技术栈、配置、架构与当前任务，生成五份初始化草稿（CONTEXT/DECISIONS/CURRENT_STATUS/PROBLEMS/CHANGELOG，无法确认的事实写入「待确认」章节，禁止猜测），调用 `{draft_create}` 创建；当前任务继续执行，不等待用户完成审核。\n\
         5. 返回 `DRAFT_PENDING_REVIEW` / `DRAFT_PARTIALLY_APPROVED` 时：草稿等待用户在 MemStack 中逐份审核，不得重复生成；需要更新时调用 `{draft_update}`（携带 expectedVersion 与变化原因）。\n\
         6. 禁止使用普通文件工具直接读取或修改 `.memstack` 目录中的正式项目文档；正式文档只能通过 `{batch_update}` 修改。\n\
         7. 每次实际项目任务结束前检查有意义变化（稳定事实、决策、当前状态、问题、重要结果）；有变化时通过 `{batch_update}` 一次提交相关文档（每份携带 expectedVersion 与 changeSummary）。普通格式调整、无行为变化的小改动、临时命令输出不写入。\n\
         8. 重大问题解决后：更新 CURRENT_STATUS / PROBLEMS / CHANGELOG 三份并通过 `{batch_update}` 一次提交，再调用 `{card_submit}` 提交结论卡片候选（必填：问题描述、最终结论、根本原因、证据、已验证结果；重要度 1-5 与评估理由必须显式给出；`cloudEmbeddingAllowed` 默认显式传 `true` 以支持语义检索，仅当用户明确禁止云端嵌入时传 `false`）。\n\
         9. MCP 不可用或令牌只读时：继续完成当前任务，并明确提示「项目记忆未更新」；不得将故障伪装为写入成功。\n\
         10. 未解决的猜测、没有证据的判断、一般性代码修改不写入项目文档，也不生成结论卡片。\n",
        handoff = tool("project_handoff_get"),
        draft_create = tool("project_document_draft_create"),
        draft_update = tool("project_document_draft_update"),
        batch_update = tool("project_document_batch_update"),
        card_submit = tool("conclusion_card_candidate_submit"),
    )
}

/// 把提示词合并进目标文件：替换既有标记片段，或追加到末尾（保留无关内容）。
fn merge_section(existing: &str, prompt_text: &str) -> String {
    let section = format!("{SECTION_BEGIN}\n\n{prompt_text}\n{SECTION_END}\n");
    if let (Some(begin), Some(end)) = (existing.find(SECTION_BEGIN), existing.find(SECTION_END))
        && begin < end
    {
        let mut merged = String::with_capacity(existing.len());
        merged.push_str(&existing[..begin]);
        merged.push_str(&section);
        merged.push_str(&existing[end + SECTION_END.len()..]);
        return merged;
    }
    if existing.trim().is_empty() {
        section
    } else {
        format!("{existing}\n\n{section}")
    }
}

fn contains_current_section(content: &str, prompt_text: &str) -> bool {
    content.contains(SECTION_BEGIN) && content.contains(SECTION_END) && content.contains(prompt_text.trim())
}

fn io_error(action: &str, path: &Path, error: std::io::Error) -> BusinessError {
    BusinessError::with_message(
        ErrorCode::InternalError,
        format!("{action}（{}）：{error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_binds_memstack_service_and_tool_names() {
        let codex = preview_prompt("codex", Path::new("C:/home"), Path::new("C:/appdata")).unwrap();
        assert_eq!(codex.service_id, "memstack");
        assert_eq!(codex.template_version, PROMPT_TEMPLATE_VERSION);
        // Codex 使用 MCP 服务前缀，避免与本地函数工具或其他服务混淆。
        assert!(codex.prompt_text.contains("`mcp__memstack__project_handoff_get`"));
        assert!(!codex.prompt_text.contains("memstack.project_handoff_get"));
        // 不含敏感信息。
        assert!(!codex.prompt_text.contains("token"));
        assert!(!codex.prompt_text.contains("C:/"));

        // Claude：实际工具名带 mcp__memstack__ 前缀。
        let claude = preview_prompt("claude", Path::new("C:/home"), Path::new("C:/appdata")).unwrap();
        assert!(claude.prompt_text.contains("`mcp__memstack__project_handoff_get`"));
        assert!(!claude.can_install, "Claude Desktop 仅查看复制");

        // Cursor：memstack_ 前缀。
        let cursor = preview_prompt("cursor", Path::new("C:/home"), Path::new("C:/appdata")).unwrap();
        assert!(cursor.prompt_text.contains("`memstack_project_handoff_get`"));
        assert!(cursor.can_install);

        // Generic：保留逻辑名 memstack.工具名。
        let generic = preview_prompt("trae", Path::new("C:/home"), Path::new("C:/appdata")).unwrap();
        assert!(generic.prompt_text.contains("`memstack.project_handoff_get`"));
        assert!(!generic.can_install);
    }

    #[test]
    fn install_is_idempotent_and_preserves_user_content() {
        let home = tempfile::tempdir().unwrap();
        let appdata = tempfile::tempdir().unwrap();
        // 预置用户已有内容。
        let agents = home.path().join(".codex").join("AGENTS.md");
        std::fs::create_dir_all(agents.parent().unwrap()).unwrap();
        std::fs::write(&agents, "# 我的个人规则\n\n- 用中文回复\n").unwrap();

        let first = install_prompt("codex", home.path(), appdata.path()).unwrap();
        assert!(first.installed);
        let content = std::fs::read_to_string(&agents).unwrap();
        // 用户内容保留 + MemStack 片段追加。
        assert!(content.contains("我的个人规则"));
        assert!(content.contains(SECTION_BEGIN));
        assert!(content.contains("MemStack 项目记忆工作流"));
        let backup = first.backup_path.unwrap();
        assert!(std::fs::read_to_string(&backup).unwrap().contains("我的个人规则"));

        // 重复安装：幂等（内容不再重复），备份不再生成（无变化）。
        let second = install_prompt("codex", home.path(), appdata.path()).unwrap();
        assert!(second.installed);
        let again = std::fs::read_to_string(&agents).unwrap();
        assert_eq!(again.matches(SECTION_BEGIN).count(), 1, "片段不得重复追加");
        assert_eq!(again.matches("MemStack 项目记忆工作流").count(), 1);

        // 预览与安装使用同一生成结果（复制即所见）。
        let preview = preview_prompt("codex", home.path(), appdata.path()).unwrap();
        assert!(preview.installed);
        assert!(again.contains(preview.prompt_text.trim()));
    }

    #[test]
    fn install_updates_stale_section() {
        let home = tempfile::tempdir().unwrap();
        let appdata = tempfile::tempdir().unwrap();
        let agents = home.path().join(".codex").join("AGENTS.md");
        std::fs::create_dir_all(agents.parent().unwrap()).unwrap();
        // 模拟旧版本片段。
        std::fs::write(
            &agents,
            format!("# 用户规则\n\n{SECTION_BEGIN}\n\n旧版提示词\n{SECTION_END}\n"),
        )
        .unwrap();
        let report = install_prompt("codex", home.path(), appdata.path()).unwrap();
        assert!(report.installed);
        let content = std::fs::read_to_string(&agents).unwrap();
        assert!(content.contains("用户规则"));
        assert!(!content.contains("旧版提示词"), "旧片段必须被替换");
        assert_eq!(content.matches(SECTION_BEGIN).count(), 1);
    }

    #[test]
    fn unsupported_client_never_writes() {
        let home = tempfile::tempdir().unwrap();
        let appdata = tempfile::tempdir().unwrap();
        let report = install_prompt("trae", home.path(), appdata.path()).unwrap();
        assert!(!report.can_install);
        assert!(!report.installed);
        assert!(report.backup_path.is_none());
        assert!(report.diff_text.contains("复制"));
        // Claude：可查看目标路径但不写入。
        let claude = install_prompt("claude", home.path(), appdata.path()).unwrap();
        assert!(!claude.can_install);
        assert!(claude.config_path.is_some());
        assert!(!claude.installed);
    }
}
