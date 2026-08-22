//! 项目全局文档与结论卡片领域契约（执行计划 §6、§7、§9）。
//!
//! - 五份项目文档：CONTEXT / DECISIONS / CURRENT_STATUS / PROBLEMS / CHANGELOG，
//!   正式 Markdown 文件是唯一事实来源，SQLite 只保存镜像与索引。
//! - 初始化草稿：正式创建前的待审核内容，不属于正式项目记忆。
//! - 结论卡片：重大问题解决后的结构化经验，候选确认后成为 SOLUTION 记忆。
//!
//! 所有枚举与 DTO 序列化为 camelCase / 大写枚举文本，与既有契约风格一致。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 文档类型
// ---------------------------------------------------------------------------

/// 五份项目文档的类型（同时是文件名与 YAML `documentType` 的唯一来源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ProjectDocumentType {
    /// `01_CONTEXT.md` 项目背景。
    #[serde(rename = "CONTEXT")]
    Context,
    /// `02_DECISIONS.md` 项目决策。
    #[serde(rename = "DECISIONS")]
    Decisions,
    /// `03_CURRENT_STATUS.md` 当前状态。
    #[serde(rename = "CURRENT_STATUS")]
    CurrentStatus,
    /// `04_PROBLEMS.md` 项目问题。
    #[serde(rename = "PROBLEMS")]
    Problems,
    /// `05_CHANGELOG.md` 变更记录。
    #[serde(rename = "CHANGELOG")]
    Changelog,
}

/// 全部文档类型（固定顺序 = 文件名序号顺序）。
pub const ALL_PROJECT_DOCUMENT_TYPES: [ProjectDocumentType; 5] = [
    ProjectDocumentType::Context,
    ProjectDocumentType::Decisions,
    ProjectDocumentType::CurrentStatus,
    ProjectDocumentType::Problems,
    ProjectDocumentType::Changelog,
];

impl ProjectDocumentType {
    /// 稳定文本形式（YAML `documentType` 值与数据库存储值）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context => "CONTEXT",
            Self::Decisions => "DECISIONS",
            Self::CurrentStatus => "CURRENT_STATUS",
            Self::Problems => "PROBLEMS",
            Self::Changelog => "CHANGELOG",
        }
    }

    /// `.memstack` 内的固定文件名（如 `01_CONTEXT.md`）。
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Context => "01_CONTEXT.md",
            Self::Decisions => "02_DECISIONS.md",
            Self::CurrentStatus => "03_CURRENT_STATUS.md",
            Self::Problems => "04_PROBLEMS.md",
            Self::Changelog => "05_CHANGELOG.md",
        }
    }

    /// 文档中文标题（界面显示）。
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Context => "项目背景",
            Self::Decisions => "项目决策",
            Self::CurrentStatus => "当前状态",
            Self::Problems => "项目问题",
            Self::Changelog => "变更记录",
        }
    }

    /// 解析文本（大小写不敏感）；未知值返回 `None`。
    pub fn parse(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_uppercase();
        ALL_PROJECT_DOCUMENT_TYPES
            .into_iter()
            .find(|kind| kind.as_str() == normalized)
    }
}

/// 校验文档类型文本是否合法（大小写不敏感）。
pub fn is_valid_project_document_type(value: &str) -> bool {
    ProjectDocumentType::parse(value).is_some()
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 初始化草稿审核状态（§8.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectDocumentReviewStatus {
    /// 待审核。
    #[serde(rename = "PENDING_REVIEW")]
    PendingReview,
    /// 已批准（批准版本记录在 approved_version）。
    #[serde(rename = "APPROVED")]
    Approved,
}

impl ProjectDocumentReviewStatus {
    /// 数据库存储文本。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingReview => "PENDING_REVIEW",
            Self::Approved => "APPROVED",
        }
    }
}

/// 正式文档镜像同步状态（§8.1 中 ACTIVE 的子状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectDocumentSyncStatus {
    /// 本地文件与数据库镜像一致。
    #[serde(rename = "SYNCED")]
    Synced,
    /// 存在待补同步的本地修改。
    #[serde(rename = "SYNC_PENDING")]
    SyncPending,
    /// 版本冲突（本地与数据库互相领先，需要用户或 AI 重新提交）。
    #[serde(rename = "CONFLICT")]
    Conflict,
    /// YAML 或正文格式错误，等待修复。
    #[serde(rename = "FORMAT_ERROR")]
    FormatError,
    /// 文件缺失，等待从数据库镜像恢复。
    #[serde(rename = "REPAIR_PENDING")]
    RepairPending,
}

impl ProjectDocumentSyncStatus {
    /// 数据库存储文本。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Synced => "SYNCED",
            Self::SyncPending => "SYNC_PENDING",
            Self::Conflict => "CONFLICT",
            Self::FormatError => "FORMAT_ERROR",
            Self::RepairPending => "REPAIR_PENDING",
        }
    }
}

// ---------------------------------------------------------------------------
// 正式文档与草稿 DTO
// ---------------------------------------------------------------------------

/// 一份正式项目文档的数据库镜像视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentItem {
    pub id: String,
    pub project_id: String,
    pub document_type: ProjectDocumentType,
    /// `.memstack` 内的相对路径（如 `01_CONTEXT.md`）。
    pub relative_path: String,
    /// 镜像内容（完整文件内容，含 YAML 头）。
    pub content: String,
    pub checksum: String,
    pub version: i64,
    /// 唯一上一版安全快照版本号。
    pub previous_version: Option<i64>,
    /// 项目文档 Embedding 开关（项目级，默认关闭）。
    pub embedding_enabled: bool,
    /// 同步状态（SYNCED / SYNC_PENDING / CONFLICT / FORMAT_ERROR / REPAIR_PENDING）。
    pub sync_status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 一份初始化草稿视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentDraftItem {
    pub id: String,
    pub project_id: String,
    pub document_type: ProjectDocumentType,
    /// `.memstack/drafts/initialization/` 内的相对路径。
    pub relative_path: String,
    pub content: String,
    pub checksum: String,
    pub version: i64,
    /// 审核状态（PENDING_REVIEW / APPROVED）。
    pub review_status: String,
    /// 批准时的版本号；与当前 version 不一致视为未批准。
    pub approved_version: Option<i64>,
    /// 最近一次内容变化原因（AI 更新或用户编辑时显式提交）。
    pub last_change_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 项目文档整体状态总览（桌面端入口展示，§21.1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentOverview {
    pub project_id: String,
    pub project_name: String,
    /// 本地工作空间绝对路径（最近一次项目文档操作记录的值；可能为 null）。
    pub workspace_path: Option<String>,
    /// NOT_INITIALIZED / DRAFT_PENDING_REVIEW / DRAFT_PARTIALLY_APPROVED /
    /// DRAFT_ALL_APPROVED / PROMOTING / ACTIVE。
    pub status: String,
    /// 五份草稿（存在时）。
    pub drafts: Vec<ProjectDocumentDraftItem>,
    /// 五份正式文档状态（存在时）。
    pub documents: Vec<ProjectDocumentState>,
    /// 项目文档 Embedding 开关。
    pub embedding_enabled: bool,
    /// 残留草稿提示（正式文档已生效但草稿目录仍存在）。
    pub has_residual_drafts: bool,
    /// 最近同步时间。
    pub last_synced_at: Option<String>,
}

/// 一份正式文档在总览中的轻量状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentState {
    pub document_type: ProjectDocumentType,
    pub relative_path: String,
    pub version: i64,
    /// 校验和摘要（前 12 位，界面展示用）。
    pub checksum_prefix: String,
    pub sync_status: String,
    pub updated_at: String,
    /// 本地文件是否存在（缺失时可从镜像恢复）。
    pub file_exists: bool,
}

// ---------------------------------------------------------------------------
// 请求 DTO
// ---------------------------------------------------------------------------

/// 单份草稿内容（draft_create 的组成项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentDraftContent {
    pub document_type: ProjectDocumentType,
    /// 完整 Markdown 内容（可不含 YAML 头，服务端负责补齐）。
    pub content: String,
}

/// 创建五份初始化草稿的请求（§9.3）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectDocumentDraftsRequest {
    pub workspace_path: String,
    pub documents: Vec<ProjectDocumentDraftContent>,
    /// 创建原因（可选，界面展示）。
    pub change_reason: Option<String>,
}

/// 更新一份初始化草稿的请求（§9.4）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProjectDocumentDraftRequest {
    pub workspace_path: String,
    pub document_type: ProjectDocumentType,
    pub expected_version: i64,
    pub content: String,
    pub change_reason: String,
}

/// 批量更新正式文档中的单份更新项（§9.5）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentUpdateItem {
    pub document_type: ProjectDocumentType,
    pub expected_version: i64,
    /// 完整 Markdown 内容（可不含 YAML 头，服务端负责补齐）。
    pub content: String,
    pub change_summary: String,
}

/// 批量更新正式文档的请求（§9.5）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentBatchUpdateRequest {
    pub workspace_path: String,
    pub updates: Vec<ProjectDocumentUpdateItem>,
}

/// 批量更新结果：每份文档的新版本与校验和。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentUpdateResultItem {
    pub document_type: ProjectDocumentType,
    pub version: i64,
    pub checksum: String,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// 项目交接（§9.2）
// ---------------------------------------------------------------------------

/// 项目交接请求：全部预算参数显式传递，不设默认值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectHandoffRequest {
    /// 本地工作空间绝对路径。
    pub workspace_path: String,
    /// Context 正文最大字符数。
    pub context_max_chars: i64,
    /// 有效决策最大条数。
    pub active_decision_max_count: i64,
    /// Current Status 正文最大字符数。
    pub current_status_max_chars: i64,
    /// 当前问题最大条数。
    pub active_problem_max_count: i64,
    /// 近期已解决问题最大条数。
    pub resolved_problem_max_count: i64,
    /// 近期变更记录最大条数。
    pub recent_changelog_max_count: i64,
    /// 相关结论卡片最大条数。
    pub related_conclusion_card_max_count: i64,
    /// 单张结论卡片正文最大字符数。
    pub conclusion_card_max_chars: i64,
}

/// 项目交接结果（按预算裁剪后的项目现场）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectHandoffResult {
    /// NOT_INITIALIZED / DRAFT_PENDING_REVIEW / DRAFT_PARTIALLY_APPROVED /
    /// DRAFT_ALL_APPROVED / PROMOTING / ACTIVE。
    pub status: String,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    /// 当前 Token 是否只读（只读时提示项目记忆未更新也不可写）。
    pub read_only: bool,
    /// 同步与恢复过程中的警告（补同步、文件恢复、格式错误等）。
    pub warnings: Vec<String>,
    /// 五份文档状态（正式文档存在时）。
    pub documents: Vec<ProjectDocumentState>,
    /// 交接正文各段（预算裁剪后）。
    pub context_text: String,
    pub active_decisions_text: String,
    pub current_status_text: String,
    pub active_problems_text: String,
    pub resolved_problems_text: String,
    pub recent_changelog_text: String,
    /// 与当前状态/问题相关的结论卡片摘要。
    pub related_conclusion_cards: Vec<ConclusionCardBrief>,
    /// 已有初始化草稿（未初始化或草稿仍在时返回，供审核界面恢复）。
    pub drafts: Vec<ProjectDocumentDraftItem>,
}

/// 结论卡片在交接中的摘要视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConclusionCardBrief {
    pub memory_id: String,
    pub title: String,
    pub problem_id: Option<String>,
    /// 卡片正文（受 conclusion_card_max_chars 裁剪）。
    pub content: String,
    pub importance: i64,
    pub resolved_at: String,
}

// ---------------------------------------------------------------------------
// 结论卡片（§7.4、§7.5）
// ---------------------------------------------------------------------------

/// 结论卡片结构化数据（候选与正式共用同一结构）。
///
/// 所有数组字段必须显式传递，无内容时传空数组。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConclusionCardPayload {
    /// 卡片标题（建议「问题 → 结论」式）。
    pub title: String,
    /// 关联问题编号（`PROB-日期-序号`），可显式为 null。
    pub problem_id: Option<String>,
    /// 问题描述（必填）。
    pub problem_description: String,
    /// 最终结论（必填）。
    pub final_conclusion: String,
    /// 根本原因（必填）。
    pub root_cause: String,
    /// 适用条件。
    pub applicable_conditions: Vec<String>,
    /// 不适用条件。
    pub not_applicable_conditions: Vec<String>,
    /// 证据（必填，至少一条）。
    pub evidence: Vec<String>,
    /// 已验证结果（必填，至少一条）。
    pub verified_results: Vec<String>,
    /// 失败方案（每条须同时说明方案与失败原因）。
    pub failed_attempts: Vec<String>,
    /// 不要重复。
    pub do_not_repeat: Vec<String>,
    /// 重新尝试条件。
    pub retry_conditions: Vec<String>,
    /// 下一步。
    pub next_steps: Vec<String>,
    /// 检索关键词。
    pub keywords: Vec<String>,
    /// 标签。
    pub tags: Vec<String>,
    /// 重要度 1-5（必填）。
    pub importance: i64,
    /// 重要度评估理由（必填）。
    pub importance_reason: String,
    /// 是否允许云端嵌入（卡片自身开关，不继承项目文档设置）。
    pub cloud_embedding_allowed: bool,
    /// 解决时间（ISO 8601）。
    pub resolved_at: String,
}

/// 提交结论卡片候选的 MCP 请求（§9.6）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConclusionCardCandidateRequest {
    /// 本地工作空间绝对路径（解析绑定项目）。
    pub workspace_path: String,
    /// 结构化卡片数据。
    pub card: ConclusionCardPayload,
}

/// 结论卡片正式记录视图（确认后可查）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConclusionCardItem {
    pub memory_id: String,
    pub title: String,
    pub problem_id: Option<String>,
    pub payload: ConclusionCardPayload,
    pub created_at: String,
    pub updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_type_maps_to_file_names() {
        assert_eq!(ProjectDocumentType::Context.file_name(), "01_CONTEXT.md");
        assert_eq!(ProjectDocumentType::Changelog.file_name(), "05_CHANGELOG.md");
        assert_eq!(ALL_PROJECT_DOCUMENT_TYPES.len(), 5);
        // 文件名两两不同（类型 ↔ 文件名一一映射）。
        let names: Vec<&str> = ALL_PROJECT_DOCUMENT_TYPES.iter().map(|kind| kind.file_name()).collect();
        for (index, name) in names.iter().enumerate() {
            assert!(!names[..index].contains(name), "文件名重复：{name}");
        }
    }

    #[test]
    fn document_type_parses_case_insensitive() {
        assert_eq!(
            ProjectDocumentType::parse("context"),
            Some(ProjectDocumentType::Context)
        );
        assert_eq!(
            ProjectDocumentType::parse("CURRENT_STATUS"),
            Some(ProjectDocumentType::CurrentStatus)
        );
        assert_eq!(ProjectDocumentType::parse("unknown"), None);
        assert!(is_valid_project_document_type("problems"));
        assert!(!is_valid_project_document_type("contexts"));
    }

    #[test]
    fn document_type_serializes_uppercase() {
        assert_eq!(
            serde_json::to_string(&ProjectDocumentType::CurrentStatus).unwrap(),
            "\"CURRENT_STATUS\""
        );
        let parsed: ProjectDocumentType = serde_json::from_str("\"CHANGELOG\"").unwrap();
        assert_eq!(parsed, ProjectDocumentType::Changelog);
    }

    #[test]
    fn review_and_sync_status_serialize_uppercase() {
        assert_eq!(
            serde_json::to_string(&ProjectDocumentReviewStatus::PendingReview).unwrap(),
            "\"PENDING_REVIEW\""
        );
        assert_eq!(ProjectDocumentSyncStatus::RepairPending.as_str(), "REPAIR_PENDING");
    }

    /// 结论卡片负载的必填字段校验语义由应用层实现；
    /// 这里固化契约：数组字段缺失时反序列化失败（必须显式传递）。
    #[test]
    fn conclusion_card_payload_requires_all_fields() {
        let full = serde_json::json!({
            "title": "标题",
            "problemId": null,
            "problemDescription": "问题描述",
            "finalConclusion": "最终结论",
            "rootCause": "根本原因",
            "applicableConditions": [],
            "notApplicableConditions": [],
            "evidence": ["证据1"],
            "verifiedResults": ["验证1"],
            "failedAttempts": [],
            "doNotRepeat": [],
            "retryConditions": [],
            "nextSteps": [],
            "keywords": [],
            "tags": [],
            "importance": 4,
            "importanceReason": "影响明显",
            "cloudEmbeddingAllowed": false,
            "resolvedAt": "2026-08-21T08:00:00Z"
        });
        let payload: ConclusionCardPayload = serde_json::from_value(full).unwrap();
        assert_eq!(payload.evidence.len(), 1);
        assert!(payload.problem_id.is_none());
        // 缺少必填字段 → 解析失败。
        let incomplete = serde_json::json!({ "title": "标题" });
        assert!(serde_json::from_value::<ConclusionCardPayload>(incomplete).is_err());
    }
}
