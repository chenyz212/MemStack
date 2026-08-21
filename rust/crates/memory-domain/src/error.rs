//! 统一业务错误契约：错误码、中文消息和结构化详情。
//!
//! 错误对象序列化为 `{ code, message, details }`，字段使用 camelCase，
//! 与现有 C# `AppException` 和前端 `ApiRequestError` 的恢复语义保持一致。
//! 错误码与默认中文消息对照 C# 全量 `throw new AppException` 清单逐字对齐；
//! 同码多场景消息（如 `MCP_PROJECT_SCOPE_DENIED`）由调用点用 `with_message` 覆盖。

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 稳定业务错误码（通用码 + C# 全量业务码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    // ---- 通用码 ----
    /// 请求参数不合法。
    #[serde(rename = "INVALID_ARGUMENT")]
    InvalidArgument,
    /// 目标资源不存在。
    #[serde(rename = "NOT_FOUND")]
    NotFound,
    /// 目标状态与请求冲突（例如乐观锁版本不一致）。
    #[serde(rename = "CONFLICT")]
    Conflict,
    /// 未提供或提供了无效的客户端身份。
    #[serde(rename = "UNAUTHORIZED")]
    Unauthorized,
    /// 客户端身份有效但无权执行该操作。
    #[serde(rename = "FORBIDDEN")]
    Forbidden,
    /// 客户端已吊销或已过期。
    #[serde(rename = "CLIENT_REVOKED_OR_EXPIRED")]
    ClientRevokedOrExpired,
    /// 数据库忙（锁等待超时）。
    #[serde(rename = "DATABASE_BUSY")]
    DatabaseBusy,
    /// 数据库结构不兼容。
    #[serde(rename = "DATABASE_INCOMPATIBLE")]
    DatabaseIncompatible,
    /// 本地服务内部错误。
    #[serde(rename = "INTERNAL_ERROR")]
    InternalError,

    // ---- 项目 / 工作空间 ----
    #[serde(rename = "PROJECT_NOT_FOUND")]
    ProjectNotFound,
    #[serde(rename = "PROJECT_NAME_INVALID")]
    ProjectNameInvalid,
    #[serde(rename = "PROJECT_NAME_EXISTS")]
    ProjectNameExists,
    #[serde(rename = "PROJECT_COLOR_INVALID")]
    ProjectColorInvalid,
    #[serde(rename = "PROJECT_ARCHIVED")]
    ProjectArchived,
    #[serde(rename = "PROJECT_NOT_ARCHIVED")]
    ProjectNotArchived,
    #[serde(rename = "PROJECT_WORKSPACE_OCCUPIED")]
    ProjectWorkspaceOccupied,
    #[serde(rename = "WORKSPACE_IDENTIFIER_INVALID")]
    WorkspaceIdentifierInvalid,
    #[serde(rename = "WORKSPACE_ALREADY_BOUND")]
    WorkspaceAlreadyBound,
    #[serde(rename = "WORKSPACE_PROJECT_ARCHIVED")]
    WorkspaceProjectArchived,

    // ---- 记忆 / 历史版本 ----
    #[serde(rename = "MEMORY_NOT_FOUND")]
    MemoryNotFound,
    #[serde(rename = "MEMORY_TITLE_INVALID")]
    MemoryTitleInvalid,
    #[serde(rename = "MEMORY_CONTENT_REQUIRED")]
    MemoryContentRequired,
    #[serde(rename = "MEMORY_CONTENT_INVALID")]
    MemoryContentInvalid,
    #[serde(rename = "MEMORY_SCOPE_INVALID")]
    MemoryScopeInvalid,
    #[serde(rename = "MEMORY_STATUS_INVALID")]
    MemoryStatusInvalid,
    #[serde(rename = "MEMORY_PROJECT_REQUIRED")]
    MemoryProjectRequired,
    #[serde(rename = "MEMORY_IMPORTANCE_INVALID")]
    MemoryImportanceInvalid,
    #[serde(rename = "MEMORY_TYPE_INVALID")]
    MemoryTypeInvalid,
    #[serde(rename = "MEMORY_SOURCE_REQUIRED")]
    MemorySourceRequired,
    #[serde(rename = "MEMORY_DUPLICATE")]
    MemoryDuplicate,
    #[serde(rename = "MEMORY_ARCHIVED_READ_ONLY")]
    MemoryArchivedReadOnly,
    #[serde(rename = "MEMORY_NOT_ARCHIVED")]
    MemoryNotArchived,
    #[serde(rename = "MEMORY_VERSION_REQUIRED")]
    MemoryVersionRequired,
    #[serde(rename = "MEMORY_VERSION_CONFLICT")]
    MemoryVersionConflict,
    #[serde(rename = "MEMORY_REVISION_NOT_FOUND")]
    MemoryRevisionNotFound,
    #[serde(rename = "MEMORY_REVISION_INVALID")]
    MemoryRevisionInvalid,
    #[serde(rename = "CURSOR_INVALID")]
    CursorInvalid,

    // ---- 候选记忆 ----
    #[serde(rename = "MEMORY_CANDIDATE_NOT_FOUND")]
    MemoryCandidateNotFound,
    #[serde(rename = "MEMORY_CANDIDATE_DUPLICATE")]
    MemoryCandidateDuplicate,
    #[serde(rename = "MEMORY_CANDIDATE_VERSION_REQUIRED")]
    MemoryCandidateVersionRequired,
    #[serde(rename = "MEMORY_CANDIDATE_VERSION_CONFLICT")]
    MemoryCandidateVersionConflict,

    // ---- MCP 身份与客户端 ----
    #[serde(rename = "MCP_AUTH_REQUIRED")]
    McpAuthRequired,
    #[serde(rename = "MCP_TOKEN_INVALID")]
    McpTokenInvalid,
    #[serde(rename = "MCP_TOKEN_NOT_FOUND")]
    McpTokenNotFound,
    #[serde(rename = "MCP_TOKEN_REVOKED")]
    McpTokenRevoked,
    #[serde(rename = "MCP_TOKEN_REGENERATE_REQUIRED")]
    McpTokenRegenerateRequired,
    #[serde(rename = "MCP_TOKEN_DECRYPT_FAILED")]
    McpTokenDecryptFailed,
    #[serde(rename = "MCP_TOKEN_OPTION_INVALID")]
    McpTokenOptionInvalid,
    #[serde(rename = "MCP_DISPLAY_NAME_INVALID")]
    McpDisplayNameInvalid,
    #[serde(rename = "MCP_CLIENT_NOT_FOUND")]
    McpClientNotFound,
    #[serde(rename = "MCP_CLIENT_DUPLICATE")]
    McpClientDuplicate,
    #[serde(rename = "MCP_PERMISSION_DENIED")]
    McpPermissionDenied,
    #[serde(rename = "MCP_PROJECT_SCOPE_DENIED")]
    McpProjectScopeDenied,
    #[serde(rename = "MCP_CONNECTION_TEST_FAILED")]
    McpConnectionTestFailed,
    #[serde(rename = "MCP_TOOL_LIST_INVALID")]
    McpToolListInvalid,
    #[serde(rename = "MCP_TOOL_CALL_FAILED")]
    McpToolCallFailed,
    #[serde(rename = "MCP_PROTOCOL_ERROR")]
    McpProtocolError,
    #[serde(rename = "MCP_RESPONSE_INVALID")]
    McpResponseInvalid,

    // ---- Embedding（阶段 4 使用，码先行固化）----
    #[serde(rename = "EMBEDDING_URL_INVALID")]
    EmbeddingUrlInvalid,
    #[serde(rename = "EMBEDDING_CONFIG_INCOMPLETE")]
    EmbeddingConfigIncomplete,
    #[serde(rename = "EMBEDDING_DIMENSIONS_INVALID")]
    EmbeddingDimensionsInvalid,
    #[serde(rename = "EMBEDDING_DIMENSIONS_MISMATCH")]
    EmbeddingDimensionsMismatch,
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl ErrorCode {
    /// 返回稳定的字符串形式，与序列化值一致。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::NotFound => "NOT_FOUND",
            Self::Conflict => "CONFLICT",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Forbidden => "FORBIDDEN",
            Self::ClientRevokedOrExpired => "CLIENT_REVOKED_OR_EXPIRED",
            Self::DatabaseBusy => "DATABASE_BUSY",
            Self::DatabaseIncompatible => "DATABASE_INCOMPATIBLE",
            Self::InternalError => "INTERNAL_ERROR",
            Self::ProjectNotFound => "PROJECT_NOT_FOUND",
            Self::ProjectNameInvalid => "PROJECT_NAME_INVALID",
            Self::ProjectNameExists => "PROJECT_NAME_EXISTS",
            Self::ProjectColorInvalid => "PROJECT_COLOR_INVALID",
            Self::ProjectArchived => "PROJECT_ARCHIVED",
            Self::ProjectNotArchived => "PROJECT_NOT_ARCHIVED",
            Self::ProjectWorkspaceOccupied => "PROJECT_WORKSPACE_OCCUPIED",
            Self::WorkspaceIdentifierInvalid => "WORKSPACE_IDENTIFIER_INVALID",
            Self::WorkspaceAlreadyBound => "WORKSPACE_ALREADY_BOUND",
            Self::WorkspaceProjectArchived => "WORKSPACE_PROJECT_ARCHIVED",
            Self::MemoryNotFound => "MEMORY_NOT_FOUND",
            Self::MemoryTitleInvalid => "MEMORY_TITLE_INVALID",
            Self::MemoryContentRequired => "MEMORY_CONTENT_REQUIRED",
            Self::MemoryContentInvalid => "MEMORY_CONTENT_INVALID",
            Self::MemoryScopeInvalid => "MEMORY_SCOPE_INVALID",
            Self::MemoryStatusInvalid => "MEMORY_STATUS_INVALID",
            Self::MemoryProjectRequired => "MEMORY_PROJECT_REQUIRED",
            Self::MemoryImportanceInvalid => "MEMORY_IMPORTANCE_INVALID",
            Self::MemoryTypeInvalid => "MEMORY_TYPE_INVALID",
            Self::MemorySourceRequired => "MEMORY_SOURCE_REQUIRED",
            Self::MemoryDuplicate => "MEMORY_DUPLICATE",
            Self::MemoryArchivedReadOnly => "MEMORY_ARCHIVED_READ_ONLY",
            Self::MemoryNotArchived => "MEMORY_NOT_ARCHIVED",
            Self::MemoryVersionRequired => "MEMORY_VERSION_REQUIRED",
            Self::MemoryVersionConflict => "MEMORY_VERSION_CONFLICT",
            Self::MemoryRevisionNotFound => "MEMORY_REVISION_NOT_FOUND",
            Self::MemoryRevisionInvalid => "MEMORY_REVISION_INVALID",
            Self::CursorInvalid => "CURSOR_INVALID",
            Self::MemoryCandidateNotFound => "MEMORY_CANDIDATE_NOT_FOUND",
            Self::MemoryCandidateDuplicate => "MEMORY_CANDIDATE_DUPLICATE",
            Self::MemoryCandidateVersionRequired => "MEMORY_CANDIDATE_VERSION_REQUIRED",
            Self::MemoryCandidateVersionConflict => "MEMORY_CANDIDATE_VERSION_CONFLICT",
            Self::McpAuthRequired => "MCP_AUTH_REQUIRED",
            Self::McpTokenInvalid => "MCP_TOKEN_INVALID",
            Self::McpTokenNotFound => "MCP_TOKEN_NOT_FOUND",
            Self::McpTokenRevoked => "MCP_TOKEN_REVOKED",
            Self::McpTokenRegenerateRequired => "MCP_TOKEN_REGENERATE_REQUIRED",
            Self::McpTokenDecryptFailed => "MCP_TOKEN_DECRYPT_FAILED",
            Self::McpTokenOptionInvalid => "MCP_TOKEN_OPTION_INVALID",
            Self::McpDisplayNameInvalid => "MCP_DISPLAY_NAME_INVALID",
            Self::McpClientNotFound => "MCP_CLIENT_NOT_FOUND",
            Self::McpClientDuplicate => "MCP_CLIENT_DUPLICATE",
            Self::McpPermissionDenied => "MCP_PERMISSION_DENIED",
            Self::McpProjectScopeDenied => "MCP_PROJECT_SCOPE_DENIED",
            Self::McpConnectionTestFailed => "MCP_CONNECTION_TEST_FAILED",
            Self::McpToolListInvalid => "MCP_TOOL_LIST_INVALID",
            Self::McpToolCallFailed => "MCP_TOOL_CALL_FAILED",
            Self::McpProtocolError => "MCP_PROTOCOL_ERROR",
            Self::McpResponseInvalid => "MCP_RESPONSE_INVALID",
            Self::EmbeddingUrlInvalid => "EMBEDDING_URL_INVALID",
            Self::EmbeddingConfigIncomplete => "EMBEDDING_CONFIG_INCOMPLETE",
            Self::EmbeddingDimensionsInvalid => "EMBEDDING_DIMENSIONS_INVALID",
            Self::EmbeddingDimensionsMismatch => "EMBEDDING_DIMENSIONS_MISMATCH",
        }
    }

    /// 返回面向用户的中文消息（默认取 C# 主场景文案；变体由调用点覆盖）。
    pub fn default_message(self) -> &'static str {
        match self {
            Self::InvalidArgument => "请求参数不合法",
            Self::NotFound => "目标不存在",
            Self::Conflict => "内容已被其他会话修改，请刷新后重试",
            Self::Unauthorized => "客户端身份无效或已过期",
            Self::Forbidden => "当前客户端无权执行该操作",
            Self::ClientRevokedOrExpired => "客户端已吊销或已过期",
            Self::DatabaseBusy => "本地数据库正忙，请稍后重试",
            Self::DatabaseIncompatible => "桌面数据库结构不兼容，请保留数据文件并联系开发者处理",
            Self::InternalError => "本地服务发生错误",
            Self::ProjectNotFound => "项目不存在",
            Self::ProjectNameInvalid => "项目名称长度必须为 1 到 80 个字符",
            Self::ProjectNameExists => "已经存在同名项目",
            Self::ProjectColorInvalid => "请选择有效的项目颜色",
            Self::ProjectArchived => "归档项目不能接收新记忆",
            Self::ProjectNotArchived => "只有已归档的项目才能彻底删除",
            Self::ProjectWorkspaceOccupied => "项目已经绑定其他工作空间",
            Self::WorkspaceIdentifierInvalid => "工作空间标识无效",
            Self::WorkspaceAlreadyBound => "该工作空间已经绑定其他项目",
            Self::WorkspaceProjectArchived => "工作空间绑定的项目已归档，请先恢复或重新绑定",
            Self::MemoryNotFound => "记忆不存在",
            Self::MemoryTitleInvalid => "记忆标题长度必须为 1 到 200 个字符",
            Self::MemoryContentRequired => "请输入需要记录的内容",
            Self::MemoryContentInvalid => "记忆正文长度必须为 1 到 200000 个字符",
            Self::MemoryScopeInvalid => "记忆范围参数无效",
            Self::MemoryStatusInvalid => "记忆状态参数无效",
            Self::MemoryProjectRequired => "项目记忆必须选择项目",
            Self::MemoryImportanceInvalid => "重要程度必须为 1 到 5",
            Self::MemoryTypeInvalid => {
                "记忆类型必须是以下之一：NOTE / PREFERENCE / DECISION / SOLUTION / FACT / CONVENTION / TASK / CONTEXT / OTHER"
            }
            Self::MemorySourceRequired => "记忆来源不能为空",
            Self::MemoryDuplicate => "相同范围内已经存在内容一致的记忆",
            Self::MemoryArchivedReadOnly => "已归档记忆不能编辑，请先恢复",
            Self::MemoryNotArchived => "仅可彻底删除已归档记忆，请先归档",
            Self::MemoryVersionRequired => "修改记忆必须携带当前版本号",
            Self::MemoryVersionConflict => "记忆已被其他操作修改，请刷新后重试",
            Self::MemoryRevisionNotFound => "记忆历史版本不存在",
            Self::MemoryRevisionInvalid => "记忆历史版本无法读取",
            Self::CursorInvalid => "分页游标格式无效",
            Self::MemoryCandidateNotFound => "候选记忆不存在",
            Self::MemoryCandidateDuplicate => "已经存在相同候选记忆",
            Self::MemoryCandidateVersionRequired => "编辑候选必须携带版本号",
            Self::MemoryCandidateVersionConflict => "候选已被其他操作修改，请刷新后重试",
            Self::McpAuthRequired => "MCP 请求尚未通过身份验证",
            Self::McpTokenInvalid => "MEMSTACK_TOKEN 无效或已过期",
            Self::McpTokenNotFound => "MCP Token 不存在",
            Self::McpTokenRevoked => "MCP Token 已吊销",
            Self::McpTokenRegenerateRequired => "旧版 Token 无法恢复，请重新生成",
            Self::McpTokenDecryptFailed => "当前 Windows 用户无法解密该 Token，请重新生成",
            Self::McpTokenOptionInvalid => "MCP Token 选项无效",
            Self::McpDisplayNameInvalid => "AI 工具名称长度必须为 1 到 80 个字符",
            Self::McpClientNotFound => "AI 工具不存在",
            Self::McpClientDuplicate => "同名 AI 工具已经存在，请改名后重试",
            Self::McpPermissionDenied => "当前 Token 仅有读取权限",
            Self::McpProjectScopeDenied => "当前 Token 只能访问绑定项目",
            Self::McpConnectionTestFailed => "MCP 连接测试失败（阶段 5 提供 stdio 实现）",
            Self::McpToolListInvalid => "MCP 工具清单与固定的 16 个工具不一致",
            Self::McpToolCallFailed => "MCP 工具调用失败",
            Self::McpProtocolError => "MCP 协议调用返回错误",
            Self::McpResponseInvalid => "MCP 返回了无法识别的协议响应",
            Self::EmbeddingUrlInvalid => "请输入有效的 API 地址",
            Self::EmbeddingConfigIncomplete => "模型名称和 API Key 不能为空",
            Self::EmbeddingDimensionsInvalid => "向量维度必须为 128 到 1536",
            Self::EmbeddingDimensionsMismatch => "模型返回维度与当前设置不一致",
        }
    }
}

/// 统一业务错误对象：`code` + `message` + `details`。
#[derive(Debug, Clone, Error, Serialize, Deserialize)]
#[error("{message} ({code})")]
pub struct BusinessError {
    /// 稳定错误码。
    pub code: ErrorCode,
    /// 面向用户的中文消息。
    pub message: String,
    /// 结构化补充信息，供前端按错误码执行恢复操作。
    #[serde(default)]
    pub details: serde_json::Value,
}

impl BusinessError {
    /// 使用错误码默认中文消息创建错误。
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            message: code.default_message().to_string(),
            details: serde_json::Value::Null,
        }
    }

    /// 使用错误码和自定义消息创建错误。
    pub fn with_message(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: serde_json::Value::Null,
        }
    }

    /// 附加结构化详情。
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_error_serializes_camel_case_contract() {
        let error = BusinessError::new(ErrorCode::Conflict).with_details(serde_json::json!({ "expectedVersion": 1 }));
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["code"], "CONFLICT");
        assert_eq!(json["message"], "内容已被其他会话修改，请刷新后重试");
        assert_eq!(json["details"]["expectedVersion"], 1);
    }

    #[test]
    fn error_code_roundtrip_keeps_stable_names() {
        let code = ErrorCode::DatabaseBusy;
        let serialized = serde_json::to_string(&code).unwrap();
        assert_eq!(serialized, "\"DATABASE_BUSY\"");
        let parsed: ErrorCode = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, code);
    }

    /// 错误码映射表：与 C# `AppException` 清单逐码对照（码串 + 主场景中文文案）。
    #[test]
    fn error_codes_match_csharp_contract_table() {
        let table: &[(ErrorCode, &str, &str)] = &[
            (ErrorCode::ProjectNotFound, "PROJECT_NOT_FOUND", "项目不存在"),
            (
                ErrorCode::ProjectNameInvalid,
                "PROJECT_NAME_INVALID",
                "项目名称长度必须为 1 到 80 个字符",
            ),
            (ErrorCode::ProjectNameExists, "PROJECT_NAME_EXISTS", "已经存在同名项目"),
            (
                ErrorCode::ProjectColorInvalid,
                "PROJECT_COLOR_INVALID",
                "请选择有效的项目颜色",
            ),
            (ErrorCode::ProjectArchived, "PROJECT_ARCHIVED", "归档项目不能接收新记忆"),
            // Rust 桌面端扩展码（C# 无对应，阶段 8 起为 Tauri 独有语义）。
            (
                ErrorCode::ProjectNotArchived,
                "PROJECT_NOT_ARCHIVED",
                "只有已归档的项目才能彻底删除",
            ),
            (
                ErrorCode::ProjectWorkspaceOccupied,
                "PROJECT_WORKSPACE_OCCUPIED",
                "项目已经绑定其他工作空间",
            ),
            (
                ErrorCode::WorkspaceIdentifierInvalid,
                "WORKSPACE_IDENTIFIER_INVALID",
                "工作空间标识无效",
            ),
            (
                ErrorCode::WorkspaceAlreadyBound,
                "WORKSPACE_ALREADY_BOUND",
                "该工作空间已经绑定其他项目",
            ),
            (
                ErrorCode::WorkspaceProjectArchived,
                "WORKSPACE_PROJECT_ARCHIVED",
                "工作空间绑定的项目已归档，请先恢复或重新绑定",
            ),
            (ErrorCode::MemoryNotFound, "MEMORY_NOT_FOUND", "记忆不存在"),
            (
                ErrorCode::MemoryTitleInvalid,
                "MEMORY_TITLE_INVALID",
                "记忆标题长度必须为 1 到 200 个字符",
            ),
            (
                ErrorCode::MemoryContentRequired,
                "MEMORY_CONTENT_REQUIRED",
                "请输入需要记录的内容",
            ),
            (
                ErrorCode::MemoryContentInvalid,
                "MEMORY_CONTENT_INVALID",
                "记忆正文长度必须为 1 到 200000 个字符",
            ),
            (
                ErrorCode::MemoryScopeInvalid,
                "MEMORY_SCOPE_INVALID",
                "记忆范围参数无效",
            ),
            (
                ErrorCode::MemoryStatusInvalid,
                "MEMORY_STATUS_INVALID",
                "记忆状态参数无效",
            ),
            (
                ErrorCode::MemoryProjectRequired,
                "MEMORY_PROJECT_REQUIRED",
                "项目记忆必须选择项目",
            ),
            (
                ErrorCode::MemoryImportanceInvalid,
                "MEMORY_IMPORTANCE_INVALID",
                "重要程度必须为 1 到 5",
            ),
            (
                ErrorCode::MemoryTypeInvalid,
                "MEMORY_TYPE_INVALID",
                "记忆类型必须是以下之一：NOTE / PREFERENCE / DECISION / SOLUTION / FACT / CONVENTION / TASK / CONTEXT / OTHER",
            ),
            (
                ErrorCode::MemorySourceRequired,
                "MEMORY_SOURCE_REQUIRED",
                "记忆来源不能为空",
            ),
            (
                ErrorCode::MemoryDuplicate,
                "MEMORY_DUPLICATE",
                "相同范围内已经存在内容一致的记忆",
            ),
            (
                ErrorCode::MemoryArchivedReadOnly,
                "MEMORY_ARCHIVED_READ_ONLY",
                "已归档记忆不能编辑，请先恢复",
            ),
            (
                ErrorCode::MemoryNotArchived,
                "MEMORY_NOT_ARCHIVED",
                "仅可彻底删除已归档记忆，请先归档",
            ),
            (
                ErrorCode::MemoryVersionRequired,
                "MEMORY_VERSION_REQUIRED",
                "修改记忆必须携带当前版本号",
            ),
            (
                ErrorCode::MemoryVersionConflict,
                "MEMORY_VERSION_CONFLICT",
                "记忆已被其他操作修改，请刷新后重试",
            ),
            (
                ErrorCode::MemoryRevisionNotFound,
                "MEMORY_REVISION_NOT_FOUND",
                "记忆历史版本不存在",
            ),
            (
                ErrorCode::MemoryRevisionInvalid,
                "MEMORY_REVISION_INVALID",
                "记忆历史版本无法读取",
            ),
            (ErrorCode::CursorInvalid, "CURSOR_INVALID", "分页游标格式无效"),
            (
                ErrorCode::MemoryCandidateNotFound,
                "MEMORY_CANDIDATE_NOT_FOUND",
                "候选记忆不存在",
            ),
            (
                ErrorCode::MemoryCandidateDuplicate,
                "MEMORY_CANDIDATE_DUPLICATE",
                "已经存在相同候选记忆",
            ),
            (
                ErrorCode::MemoryCandidateVersionRequired,
                "MEMORY_CANDIDATE_VERSION_REQUIRED",
                "编辑候选必须携带版本号",
            ),
            (
                ErrorCode::MemoryCandidateVersionConflict,
                "MEMORY_CANDIDATE_VERSION_CONFLICT",
                "候选已被其他操作修改，请刷新后重试",
            ),
            (
                ErrorCode::McpAuthRequired,
                "MCP_AUTH_REQUIRED",
                "MCP 请求尚未通过身份验证",
            ),
            (
                ErrorCode::McpTokenInvalid,
                "MCP_TOKEN_INVALID",
                "MEMSTACK_TOKEN 无效或已过期",
            ),
            (ErrorCode::McpTokenNotFound, "MCP_TOKEN_NOT_FOUND", "MCP Token 不存在"),
            (ErrorCode::McpTokenRevoked, "MCP_TOKEN_REVOKED", "MCP Token 已吊销"),
            (
                ErrorCode::McpTokenRegenerateRequired,
                "MCP_TOKEN_REGENERATE_REQUIRED",
                "旧版 Token 无法恢复，请重新生成",
            ),
            (
                ErrorCode::McpTokenDecryptFailed,
                "MCP_TOKEN_DECRYPT_FAILED",
                "当前 Windows 用户无法解密该 Token，请重新生成",
            ),
            (
                ErrorCode::McpTokenOptionInvalid,
                "MCP_TOKEN_OPTION_INVALID",
                "MCP Token 选项无效",
            ),
            (
                ErrorCode::McpDisplayNameInvalid,
                "MCP_DISPLAY_NAME_INVALID",
                "AI 工具名称长度必须为 1 到 80 个字符",
            ),
            (ErrorCode::McpClientNotFound, "MCP_CLIENT_NOT_FOUND", "AI 工具不存在"),
            (
                ErrorCode::McpClientDuplicate,
                "MCP_CLIENT_DUPLICATE",
                "同名 AI 工具已经存在，请改名后重试",
            ),
            (
                ErrorCode::McpPermissionDenied,
                "MCP_PERMISSION_DENIED",
                "当前 Token 仅有读取权限",
            ),
            (
                ErrorCode::McpProjectScopeDenied,
                "MCP_PROJECT_SCOPE_DENIED",
                "当前 Token 只能访问绑定项目",
            ),
            (
                ErrorCode::McpToolListInvalid,
                "MCP_TOOL_LIST_INVALID",
                "MCP 工具清单与固定的 16 个工具不一致",
            ),
            (
                ErrorCode::McpProtocolError,
                "MCP_PROTOCOL_ERROR",
                "MCP 协议调用返回错误",
            ),
            (
                ErrorCode::McpResponseInvalid,
                "MCP_RESPONSE_INVALID",
                "MCP 返回了无法识别的协议响应",
            ),
            (
                ErrorCode::EmbeddingUrlInvalid,
                "EMBEDDING_URL_INVALID",
                "请输入有效的 API 地址",
            ),
            (
                ErrorCode::EmbeddingConfigIncomplete,
                "EMBEDDING_CONFIG_INCOMPLETE",
                "模型名称和 API Key 不能为空",
            ),
            (
                ErrorCode::EmbeddingDimensionsInvalid,
                "EMBEDDING_DIMENSIONS_INVALID",
                "向量维度必须为 128 到 1536",
            ),
            (
                ErrorCode::EmbeddingDimensionsMismatch,
                "EMBEDDING_DIMENSIONS_MISMATCH",
                "模型返回维度与当前设置不一致",
            ),
        ];
        for (code, code_text, message) in table {
            assert_eq!(code.as_str(), *code_text);
            assert_eq!(code.default_message(), *message);
            let serialized = serde_json::to_string(code).unwrap();
            assert_eq!(serialized, format!("\"{code_text}\""));
        }
    }
}
