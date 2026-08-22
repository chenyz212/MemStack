//! 项目文档文件系统规则：路径常量、YAML 校验、安全写入与 Git 本地排除（执行计划 §5、§6、§12）。
//!
//! - 正式 Markdown 文件是唯一事实来源；本模块集中管理 `.memstack` 目录结构。
//! - 所有文件名来自 `ProjectDocumentType` 常量映射，杜绝路径逃逸。
//! - 写入采用「同目录临时文件 + 原子重命名」，临时文件名由常量前缀统一，
//!   完成或失败后清理，且永不匹配五份正式文件名（不进入交接与索引）。

use std::path::{Path, PathBuf};

use memory_domain::{BusinessError, ErrorCode, ProjectDocumentType, content_checksum};

/// `.memstack` 目录名。
pub const MEMSTACK_DIR_NAME: &str = ".memstack";
/// 初始化草稿目录（相对 `.memstack`）。
pub const DRAFTS_RELATIVE_DIR: &str = "drafts/initialization";
/// 同目录临时文件前缀（原子替换暂存；不匹配任何正式文件名）。
pub const TEMP_FILE_PREFIX: &str = ".memstack-tmp-";
/// 正式文档批量更新恢复日志文件名。
pub const BATCH_UPDATE_JOURNAL_FILE_NAME: &str = ".batch-update-journal.json";
/// Git 本地排除文件内容。
const GIT_EXCLUDE_LINE: &str = "/.memstack/";
/// 当前支持的文档 schema 版本。
pub const SUPPORTED_DOCUMENT_SCHEMA_VERSION: i64 = 1;

// ---------------------------------------------------------------------------
// 工作空间路径
// ---------------------------------------------------------------------------

/// 一个工作空间的 `.memstack` 目录结构（全部路径由常量派生）。
#[derive(Debug, Clone)]
pub struct WorkspacePaths {
    /// 规范化后的工作空间根目录（解析符号链接）。
    root: PathBuf,
}

impl WorkspacePaths {
    /// 规范化并校验工作空间路径：必须是非空绝对路径且真实存在的目录。
    ///
    /// `canonicalize` 解析符号链接与盘符别名，越界链接在此处被收敛到真实路径。
    pub fn resolve(workspace_path: &str) -> Result<Self, BusinessError> {
        let invalid = |detail: &str| {
            BusinessError::with_message(
                ErrorCode::WorkspaceIdentifierInvalid,
                format!("工作空间路径无效：{detail}"),
            )
        };
        let trimmed = workspace_path.trim();
        if trimmed.is_empty() {
            return Err(invalid("不能为空"));
        }
        let absolute =
            std::path::absolute(trimmed).map_err(|error| invalid(&format!("无法解析为绝对路径：{error}")))?;
        if !absolute.is_dir() {
            return Err(invalid(&format!("目录不存在：{}", absolute.display())));
        }
        let root = absolute
            .canonicalize()
            .map_err(|error| invalid(&format!("无法规范化路径：{error}")))?;
        Ok(Self { root })
    }

    /// 工作空间根目录。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `.memstack` 目录。
    pub fn memstack_dir(&self) -> PathBuf {
        self.root.join(MEMSTACK_DIR_NAME)
    }

    /// 初始化草稿目录。
    pub fn drafts_dir(&self) -> PathBuf {
        self.memstack_dir().join(DRAFTS_RELATIVE_DIR)
    }

    /// 草稿根目录（`.memstack/drafts`；晋升成功后整体删除）。
    pub fn drafts_root(&self) -> PathBuf {
        self.memstack_dir().join("drafts")
    }

    /// 一份正式文档的绝对路径（`<root>/.memstack/01_CONTEXT.md`）。
    pub fn document_path(&self, document_type: ProjectDocumentType) -> PathBuf {
        self.memstack_dir().join(document_type.file_name())
    }

    /// 正式文档批量更新恢复日志路径。
    pub fn batch_update_journal_path(&self) -> PathBuf {
        self.memstack_dir().join(BATCH_UPDATE_JOURNAL_FILE_NAME)
    }

    /// 一份初始化草稿的绝对路径。
    pub fn draft_path(&self, document_type: ProjectDocumentType) -> PathBuf {
        self.drafts_dir().join(document_type.file_name())
    }

    /// 确保 `.memstack` 与草稿目录存在。
    pub fn ensure_memstack_dirs(&self) -> Result<(), BusinessError> {
        create_dir_all(&self.drafts_dir())
    }

    /// 幂等维护 Git 本地排除：仓库存在时把 `/.memstack/` 写入 `.git/info/exclude`。
    ///
    /// 只写 `.git/info/exclude`（本地生效、无需提交）；非 Git 工作空间跳过。
    pub fn ensure_git_exclude(&self) -> Result<(), BusinessError> {
        let git_dir = self.root.join(".git");
        if !git_dir.is_dir() {
            return Ok(());
        }
        let info_dir = git_dir.join("info");
        create_dir_all(&info_dir)?;
        let exclude = info_dir.join("exclude");
        let existing = read_text_if_exists(&exclude)?.unwrap_or_default();
        for line in existing.lines() {
            if line.trim() == GIT_EXCLUDE_LINE {
                return Ok(());
            }
        }
        let mut appended = existing;
        if !appended.is_empty() && !appended.ends_with('\n') {
            appended.push('\n');
        }
        appended.push_str(GIT_EXCLUDE_LINE);
        appended.push('\n');
        std::fs::write(&exclude, appended).map_err(|error| io_error("写入 Git 本地排除失败", &exclude, error))
    }
}

// ---------------------------------------------------------------------------
// 文档渲染与解析
// ---------------------------------------------------------------------------

/// 渲染完整文档内容：最小 YAML 头 + 正文。
///
/// 易变字段（projectId/version/checksum 等）只存数据库，不写入 Markdown（§6.1）。
pub fn render_document(document_type: ProjectDocumentType, body: &str) -> String {
    let normalized_body = body.trim_start_matches('\n');
    format!(
        "---\nmemstack: PROJECT_DOCUMENT\ndocumentType: {}\nschemaVersion: {}\n---\n\n{}\n",
        document_type.as_str(),
        SUPPORTED_DOCUMENT_SCHEMA_VERSION,
        normalized_body.trim_end()
    )
}

/// 解析后的文档：类型、版本与正文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDocument {
    pub document_type: ProjectDocumentType,
    pub schema_version: i64,
    /// 正文（不含 YAML 头；保证非空）。
    pub body: String,
}

/// 严格解析并校验文档内容：YAML 头、类型匹配、schema 版本与正文非空。
pub fn parse_document(expected: ProjectDocumentType, content: &str) -> Result<ParsedDocument, BusinessError> {
    let format_error = |detail: String| {
        BusinessError::with_message(
            ErrorCode::ProjectDocumentFormatInvalid,
            format!("{}（{}）", detail, expected.file_name()),
        )
    };
    let mut lines = content.lines();
    let Some(first) = lines.next() else {
        return Err(format_error("文件为空".to_string()));
    };
    if first.trim() != "---" {
        return Err(format_error("缺少 YAML 头起始行 `---`".to_string()));
    }
    let mut memstack: Option<String> = None;
    let mut document_type: Option<String> = None;
    let mut schema_version: Option<i64> = None;
    let mut header_end = None;
    for (index, line) in lines.enumerate() {
        if line.trim() == "---" {
            header_end = Some(index);
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format_error(format!("YAML 头存在无法解析的行：`{line}`")));
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "memstack" => memstack = Some(value.to_string()),
            "documentType" => document_type = Some(value.to_string()),
            "schemaVersion" => {
                schema_version = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| format_error(format!("schemaVersion 不是整数：`{value}`")))?,
                );
            }
            _ => {
                return Err(format_error(format!("YAML 头包含未知字段：`{key}`")));
            }
        }
    }
    let Some(_) = header_end else {
        return Err(format_error("YAML 头缺少结束行 `---`".to_string()));
    };
    if memstack.as_deref() != Some("PROJECT_DOCUMENT") {
        return Err(format_error("YAML 头缺少 `memstack: PROJECT_DOCUMENT`".to_string()));
    }
    let actual_type = document_type
        .as_deref()
        .and_then(ProjectDocumentType::parse)
        .ok_or_else(|| {
            format_error(format!(
                "documentType 无效：`{}`（期望 `{}`）",
                document_type.as_deref().unwrap_or(""),
                expected.as_str()
            ))
        })?;
    if actual_type != expected {
        return Err(format_error(format!(
            "documentType 与文件名不匹配：文件 {} 声明 {}",
            expected.file_name(),
            actual_type.as_str()
        )));
    }
    if schema_version != Some(SUPPORTED_DOCUMENT_SCHEMA_VERSION) {
        return Err(format_error(format!(
            "schemaVersion 不受支持：`{}`（当前支持 {SUPPORTED_DOCUMENT_SCHEMA_VERSION}）",
            schema_version.unwrap_or_default()
        )));
    }
    let body_start = content
        .find("---\n")
        .and_then(|first| content[first + 4..].find("---\n").map(|second| first + 4 + second + 4))
        .ok_or_else(|| format_error("无法定位正文起始位置".to_string()))?;
    let body = content[body_start..].trim();
    if body.is_empty() {
        return Err(format_error("正文为空".to_string()));
    }
    Ok(ParsedDocument {
        document_type: actual_type,
        schema_version: SUPPORTED_DOCUMENT_SCHEMA_VERSION,
        body: body.to_string(),
    })
}

/// 尽力提取正文（自动修复用，§15.2）：YAML 损坏时保留可识别的正文。
///
/// 返回 `None` 表示正文无法可靠提取（只能从数据库镜像恢复完整文件）。
pub fn extract_body_lossy(content: &str) -> Option<String> {
    let mut separator_count = 0;
    for (index, line) in content.lines().enumerate() {
        if line.trim() == "---" {
            separator_count += 1;
            if separator_count == 2 {
                let body: String = content
                    .lines()
                    .skip(index + 1)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string();
                return if body.is_empty() { None } else { Some(body) };
            }
        }
    }
    // 完全没有 YAML 头：整个内容视为正文（至少 1 行非空才可靠）。
    let trimmed = content.trim();
    (!trimmed.is_empty() && !trimmed.starts_with("---")).then(|| trimmed.to_string())
}

/// 计算完整文件内容（含 YAML 头）的校验和（规范化 + SHA-256，与记忆校验同规则）。
pub fn document_checksum(content: &str) -> String {
    content_checksum(content)
}

// ---------------------------------------------------------------------------
// 文件读写
// ---------------------------------------------------------------------------

/// 读取文本文件；文件不存在返回 `None`。
pub fn read_text_if_exists(path: &Path) -> Result<Option<String>, BusinessError> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|error| io_error("读取文件失败", path, error))
}

/// 安全写入：同目录临时文件 + 原子重命名（同卷 rename 原子替换既有文件）。
pub fn atomic_write(path: &Path, content: &str) -> Result<(), BusinessError> {
    let parent = path.parent().ok_or_else(|| {
        BusinessError::with_message(ErrorCode::InternalError, format!("路径缺少父目录：{}", path.display()))
    })?;
    create_dir_all(parent)?;
    let unique = format!(
        "{TEMP_FILE_PREFIX}{}-{}",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("doc"),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    );
    let temp_path = parent.join(unique);
    if let Err(error) = std::fs::write(&temp_path, content) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(io_error("写入临时文件失败", &temp_path, error));
    }
    if let Err(error) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(io_error("原子替换文件失败", path, error));
    }
    Ok(())
}

/// 删除路径（文件或目录）；不存在视为成功。
pub fn remove_if_exists(path: &Path) -> Result<(), BusinessError> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
            .or_else(|error| if path.exists() { Err(error) } else { Ok(()) })
            .map_err(|error| io_error("删除目录失败", path, error))
    } else if path.exists() {
        std::fs::remove_file(path).map_err(|error| io_error("删除文件失败", path, error))
    } else {
        Ok(())
    }
}

/// 清理残留临时文件（晋升恢复 / 启动自愈调用）。
pub fn cleanup_temp_files(memstack_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(memstack_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if let Some(name) = name.to_str()
            && name.starts_with(TEMP_FILE_PREFIX)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn create_dir_all(path: &Path) -> Result<(), BusinessError> {
    std::fs::create_dir_all(path).map_err(|error| io_error("创建目录失败", path, error))
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
    use memory_domain::ALL_PROJECT_DOCUMENT_TYPES;

    #[test]
    fn render_and_parse_roundtrip() {
        for kind in ALL_PROJECT_DOCUMENT_TYPES {
            let content = render_document(kind, "# 标题\n\n正文内容。");
            let parsed = parse_document(kind, &content).unwrap();
            assert_eq!(parsed.document_type, kind);
            assert_eq!(parsed.schema_version, 1);
            assert_eq!(parsed.body, "# 标题\n\n正文内容。");
        }
    }

    #[test]
    fn parse_rejects_type_mismatch_and_bad_yaml() {
        let content = render_document(ProjectDocumentType::Context, "# 背景");
        let error = parse_document(ProjectDocumentType::Decisions, &content).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentFormatInvalid);
        assert!(error.message.contains("不匹配"));

        // 缺少结束分隔符。
        let broken = "---\nmemstack: PROJECT_DOCUMENT\n";
        assert!(parse_document(ProjectDocumentType::Context, broken).is_err());

        // schemaVersion 不受支持。
        let unsupported = "---\nmemstack: PROJECT_DOCUMENT\ndocumentType: CONTEXT\nschemaVersion: 2\n---\n\n正文";
        let error = parse_document(ProjectDocumentType::Context, unsupported).unwrap_err();
        assert!(error.message.contains("不受支持"));

        // 未知字段。
        let unknown = "---\nmemstack: PROJECT_DOCUMENT\ndocumentType: CONTEXT\nschemaVersion: 1\nextra: x\n---\n\n正文";
        assert!(parse_document(ProjectDocumentType::Context, unknown).is_err());

        // 正文为空。
        let empty_body = "---\nmemstack: PROJECT_DOCUMENT\ndocumentType: CONTEXT\nschemaVersion: 1\n---\n\n";
        assert!(parse_document(ProjectDocumentType::Context, empty_body).is_err());
    }

    #[test]
    fn lossy_extraction_keeps_body() {
        // YAML 头损坏但正文完整。
        let broken = "---\nmemstack: PROJECT\nbroken...\n---\n\n# 正文标题\n\n内容";
        assert_eq!(extract_body_lossy(broken).as_deref(), Some("# 正文标题\n\n内容"));
        // 完全没有头。
        assert_eq!(extract_body_lossy("# 只有正文").as_deref(), Some("# 只有正文"));
        // 只有头没有正文 → 不可靠。
        assert_eq!(extract_body_lossy("---\nmemstack: PROJECT_DOCUMENT\n---\n"), None);
        assert_eq!(extract_body_lossy(""), None);
    }

    #[test]
    fn atomic_write_replaces_existing_and_cleans_temp() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("01_CONTEXT.md");
        atomic_write(&target, "第一版").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第一版");
        atomic_write(&target, "第二版").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第二版");
        // 无临时文件残留。
        let names: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["01_CONTEXT.md".to_string()]);
        cleanup_temp_files(temp.path());
    }

    #[test]
    fn git_exclude_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::resolve(temp.path().to_str().unwrap()).unwrap();
        // 非 Git 工作空间：跳过。
        paths.ensure_git_exclude().unwrap();
        assert!(!temp.path().join(".git/info/exclude").exists());
        // 变成 Git 仓库后：写入且幂等。
        std::fs::create_dir_all(temp.path().join(".git/info")).unwrap();
        std::fs::write(temp.path().join(".git/info/exclude"), "/target/\n").unwrap();
        paths.ensure_git_exclude().unwrap();
        let content = std::fs::read_to_string(temp.path().join(".git/info/exclude")).unwrap();
        assert_eq!(content, "/target/\n/.memstack/\n");
        // 再次执行不重复追加，且保留用户已有内容。
        paths.ensure_git_exclude().unwrap();
        let again = std::fs::read_to_string(temp.path().join(".git/info/exclude")).unwrap();
        assert_eq!(again, content);
    }

    #[test]
    fn workspace_paths_reject_missing_directory() {
        let error = WorkspacePaths::resolve("").unwrap_err();
        assert_eq!(error.code, ErrorCode::WorkspaceIdentifierInvalid);
        let error = WorkspacePaths::resolve(r"E:\AICoding\definitely-missing-dir").unwrap_err();
        assert!(error.message.contains("目录不存在"));
    }

    #[test]
    fn workspace_paths_derive_fixed_layout() {
        let temp = tempfile::tempdir().unwrap();
        let paths = WorkspacePaths::resolve(temp.path().to_str().unwrap()).unwrap();
        assert_eq!(
            paths.document_path(ProjectDocumentType::Context),
            temp.path().canonicalize().unwrap().join(".memstack/01_CONTEXT.md")
        );
        assert_eq!(
            paths.draft_path(ProjectDocumentType::Changelog),
            temp.path()
                .canonicalize()
                .unwrap()
                .join(".memstack/drafts/initialization/05_CHANGELOG.md")
        );
    }
}
