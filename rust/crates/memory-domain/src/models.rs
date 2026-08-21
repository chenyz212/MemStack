//! 领域实体与 DTO 契约。
//!
//! 枚举值保持与 C# 一致的大小写（`Personal`、`Project`、`Active`、`Archived`），
//! JSON 字段使用 camelCase（`JsonSerializerDefaults.Web`），时间使用可排序的 UTC ISO 8601 文本。
//! 可空字段序列化为 `null`（与 C# System.Text.Json 默认行为一致，不忽略 null），
//! 保证跨语言契约对比可做到字节级相等。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 兼容 C# `JsonStringEnumConverter` 的枚举：序列化为名称字符串，反序列化
/// 同时接受名称字符串与 C# 默认整数（读取历史 `snapshot_json` 整数数据）。
macro_rules! flexible_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident($text:expr, $number:expr)),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        pub enum $name {
            $($variant),+
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                match serde_json::Value::deserialize(deserializer)? {
                    $(serde_json::Value::String(text) if text == $text => Ok(Self::$variant),)+
                    $(serde_json::Value::Number(number) if number.as_i64() == Some($number) => Ok(Self::$variant),)+
                    value => Err(serde::de::Error::custom(format!(
                        concat!("无效的 ", stringify!($name), " 值：{}"), value))),
                }
            }
        }
    };
}

flexible_enum! {
    /// 记忆作用域，取值与 C# `MemoryScope` 完全一致。
    MemoryScope {
        Personal("Personal", 0),
        Project("Project", 1),
    }
}

impl MemoryScope {
    /// 数据库存储文本（与 C# `ToString()` 一致）。
    pub fn as_scope_text(self) -> &'static str {
        match self {
            Self::Personal => "Personal",
            Self::Project => "Project",
        }
    }
}

flexible_enum! {
    /// 记忆状态，取值与 C# 一致。
    MemoryStatus {
        Active("Active", 0),
        Archived("Archived", 1),
    }
}

impl MemoryStatus {
    /// 数据库存储文本（与 C# `ToString()` 一致）。
    pub fn as_status_text(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Archived => "Archived",
        }
    }
}

flexible_enum! {
    /// MCP 客户端权限，取值与 C# `McpPermission` 一致。
    McpPermission {
        Read("Read", 0),
        ReadWrite("ReadWrite", 1),
    }
}

flexible_enum! {
    /// 连接页面支持的 AI 助手类型（旧数据兼容字段，与 C# `McpAssistantType` 一致）。
    McpAssistantType {
        Codex("Codex", 0),
        Claude("Claude", 1),
        Cursor("Cursor", 2),
        Trae("Trae", 3),
        Generic("Generic", 4),
    }
}

flexible_enum! {
    /// 动态 MCP 客户端会话的连接状态（派生计算，与 C# `McpClientStatus` 一致）。
    McpClientStatus {
        Active("Active", 0),
        Revoked("Revoked", 1),
        Expired("Expired", 2),
    }
}

// ---------------------------------------------------------------------------
// 记忆类型常量
// ---------------------------------------------------------------------------

/// 记忆类型合法枚举值（与前端 memoryTypeLabels 完全对齐）。
pub const VALID_MEMORY_TYPES: &[&str] = &[
    "NOTE",       // 笔记
    "PREFERENCE", // 偏好
    "DECISION",   // 决策
    "SOLUTION",   // 方案
    "FACT",       // 事实
    "CONVENTION", // 约定
    "TASK",       // 任务
    "CONTEXT",    // 上下文
    "OTHER",      // 其他
];

/// 校验 memory_type 是否属于合法枚举（大小写不敏感，输入会被 to_uppercase 归一化）。
pub fn is_valid_memory_type(memory_type: &str) -> bool {
    let normalized = memory_type.to_uppercase();
    VALID_MEMORY_TYPES.contains(&normalized.as_str())
}

// ---------------------------------------------------------------------------
// 项目
// ---------------------------------------------------------------------------

/// 项目 DTO（字段与 C# `ProjectItem` camelCase 输出一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub is_archived: bool,
    pub workspace_bound: bool,
    pub workspace_identifier: Option<String>,
    pub active_memory_count: i64,
    /// 全部记忆数（含已归档），供已归档项目彻底删除确认显示。
    pub total_memory_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// 彻底删除已归档项目的统计（Rust 桌面端扩展，供前端结果提示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletedProjectStats {
    /// 物理删除的记忆条数（含已归档）。
    pub deleted_memories: i64,
    /// 物理删除的候选记忆条数。
    pub deleted_candidates: i64,
}

/// 项目保存请求（与 C# `SaveProjectRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveProjectRequest {
    pub name: String,
    pub description: String,
    pub color: String,
}

// ---------------------------------------------------------------------------
// 记忆
// ---------------------------------------------------------------------------

/// 记忆条目 DTO（字段与 C# `MemoryItem` camelCase 输出一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryItem {
    pub id: String,
    pub scope: MemoryScope,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub memory_type: String,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    pub importance: i64,
    pub is_favorite: bool,
    pub is_pinned: bool,
    pub cloud_processing_allowed: bool,
    pub status: MemoryStatus,
    pub version: i64,
    pub created_source: String,
    pub updated_source: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

/// 记忆保存请求（与 C# `SaveMemoryRequest` 一致，含乐观锁版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMemoryRequest {
    pub scope: MemoryScope,
    pub project_id: Option<String>,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub memory_type: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub importance: i64,
    #[serde(default)]
    pub is_favorite: bool,
    #[serde(default)]
    pub is_pinned: bool,
    #[serde(default)]
    pub cloud_processing_allowed: bool,
    pub expected_version: Option<i64>,
}

/// 快速记录请求（与 C# `QuickCaptureRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickCaptureRequest {
    pub content: String,
}

/// 记忆历史版本 DTO（与 C# `MemoryRevisionItem` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRevisionItem {
    pub id: String,
    pub memory_id: String,
    pub version: i64,
    pub created_at: String,
}

/// 记忆列表筛选条件（与 C# `MemoryListQuery` 一致；缺省语义对齐端点层：
/// status 缺省 = Active，size 缺省 = 30，见 ApiEndpoints.ListMemoriesAsync）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryListQuery {
    pub scope: Option<MemoryScope>,
    pub project_id: Option<String>,
    #[serde(default)]
    pub status: Option<MemoryStatus>,
    pub is_favorite: Option<bool>,
    pub is_pinned: Option<bool>,
    pub memory_type: Option<String>,
    pub tag: Option<String>,
    pub importance_min: Option<i64>,
    pub cursor: Option<String>,
    #[serde(default = "default_page_size")]
    pub size: i64,
}

/// 记忆列表默认分页大小（与 C# 端点 `size ?? 30` 一致）。
fn default_page_size() -> i64 {
    30
}

/// 记忆页面各分类和项目的数量（与 C# `MemoryFacets` 一致；项目计数键为 GUID 字符串）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFacets {
    pub all_count: i64,
    pub personal_count: i64,
    pub project_count: i64,
    pub favorite_count: i64,
    pub pinned_count: i64,
    pub archived_count: i64,
    pub project_counts: BTreeMap<String, i64>,
}

// ---------------------------------------------------------------------------
// 工作空间
// ---------------------------------------------------------------------------

/// 待解析的工作空间请求（与 C# `ResolveWorkspaceRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveWorkspaceRequest {
    pub workspace_identifier: String,
}

/// 工作空间解析结果（与 C# `WorkspaceResolution` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceResolution {
    pub status: String,
    pub workspace_identifier: String,
    pub project: Option<ProjectItem>,
    pub requires_user_input: bool,
    pub question: Option<String>,
}

/// 项目工作空间绑定请求（与 C# `BindWorkspaceRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BindWorkspaceRequest {
    pub workspace_identifier: String,
}

/// 由 AI 提交的工作空间记忆（与 C# `WorkspaceMemoryRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMemoryRequest {
    pub workspace_identifier: String,
    pub project_name: Option<String>,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub memory_type: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub importance: i64,
    #[serde(default)]
    pub cloud_processing_allowed: bool,
}

/// 工作空间记忆保存结果（与 C# `WorkspaceMemoryResult` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMemoryResult {
    pub status: String,
    pub requires_user_input: bool,
    pub workspace_identifier: String,
    pub question: Option<String>,
    pub project: Option<ProjectItem>,
    pub memory: Option<MemoryItem>,
}

// ---------------------------------------------------------------------------
// MCP 客户端与 Token
// ---------------------------------------------------------------------------

/// 动态 MCP 客户端会话的安全卡片视图（与 C# `McpClientCard` 一致）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpClientCard {
    pub session_id: String,
    pub client_key: String,
    pub display_name: String,
    pub client_version: Option<String>,
    pub transport: String,
    pub token_prefix: String,
    pub permission: McpPermission,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub call_count: i64,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub status: McpClientStatus,
}

/// 创建动态 AI 工具的请求（与 C# `CreateMcpClientRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMcpClientRequest {
    pub display_name: String,
    pub permission: McpPermission,
    pub project_id: Option<String>,
    pub expires_at: Option<String>,
}

/// 更新动态 AI 工具的请求（与 C# `UpdateMcpClientRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMcpClientRequest {
    pub display_name: String,
    pub permission: McpPermission,
    pub project_id: Option<String>,
    pub expires_at: Option<String>,
    #[serde(default)]
    pub clear_expires_at: bool,
}

/// 动态客户端会话的完整视图（含明文 Token，与 C# `McpClientSecret` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpClientSecret {
    pub client: McpClientCard,
    pub plain_token: String,
}

/// 本机 MCP 连接信息（与 C# `McpConnectionInfo` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConnectionInfo {
    pub endpoint: String,
    pub protocol_version: String,
    pub status: String,
    pub error_message: Option<String>,
}

/// MCP 访问令牌的安全列表视图（与 C# `McpTokenItem` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTokenItem {
    pub id: String,
    pub assistant_type: McpAssistantType,
    pub display_name: String,
    pub token_prefix: String,
    pub permission: McpPermission,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

/// 创建 MCP Token 的显式请求（与 C# `CreateMcpTokenRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateMcpTokenRequest {
    pub assistant_type: McpAssistantType,
    pub display_name: String,
    pub permission: McpPermission,
    pub project_id: Option<String>,
    pub expires_at: Option<String>,
}

/// 新建或重新生成后的完整 Token（与 C# `McpTokenSecret` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTokenSecret {
    pub token: McpTokenItem,
    pub plain_token: String,
}

/// 一次已经通过验证的 MCP 调用身份（与 C# `McpCallerContext` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCallerContext {
    pub token_id: String,
    pub display_name: String,
    pub permission: McpPermission,
    pub project_id: Option<String>,
}

// ---------------------------------------------------------------------------
// 候选记忆
// ---------------------------------------------------------------------------

/// 尚未进入正式记忆库的 AI 候选记忆（与 C# `MemoryCandidateItem` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCandidateItem {
    pub id: String,
    pub scope: MemoryScope,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub memory_type: String,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    pub importance: i64,
    pub cloud_processing_allowed: bool,
    pub source_name: String,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// 候选记忆的完整保存字段（与 C# `SaveMemoryCandidateRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMemoryCandidateRequest {
    pub scope: MemoryScope,
    pub project_id: Option<String>,
    pub title: String,
    pub summary: String,
    pub content: String,
    pub memory_type: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub importance: i64,
    #[serde(default)]
    pub cloud_processing_allowed: bool,
    pub expected_version: Option<i64>,
}

/// 确认或拒绝候选时需要的乐观锁版本（与 C# `CandidateDecisionRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDecisionRequest {
    pub expected_version: i64,
}

// ---------------------------------------------------------------------------
// 分页与总览
// ---------------------------------------------------------------------------

/// 游标分页结果（与 C# `CursorPage<T>` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// 总览聚合数据（与 C# `OverviewResult` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewResult {
    pub memory_count: i64,
    pub project_count: i64,
    pub candidate_count: i64,
    pub recent_memories: Vec<MemoryItem>,
    pub active_projects: Vec<ProjectItem>,
    pub search_mode: String,
    pub mcp_status: String,
    pub recent_assistant_name: Option<String>,
    pub last_mcp_call_at: Option<String>,
    /// 最近一次 MCP 读取/创建活动（v8 起 mcp_token 记录，供总览活动文案）。
    #[serde(default)]
    pub mcp_activity: Option<McpActivitySummary>,
    /// 未吊销、未过期的 AI 工具（按最近使用排序，最多 6 个），供总览双层轨道。
    #[serde(default)]
    pub recent_clients: Vec<OverviewClient>,
    /// 符合条件的 AI 工具总数（供「+N」角标）。
    #[serde(default)]
    pub active_client_count: i64,
}

/// MCP 最近记忆活动摘要（不含项目名、标题与正文）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpActivitySummary {
    pub display_name: String,
    /// `READ` / `CREATE`。
    pub action: String,
    /// `Personal` / `Project` / `Mixed`。
    pub scope: String,
    pub occurred_at: String,
}

/// 总览轨道上的 AI 工具（名称供前端目录匹配字母与颜色）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewClient {
    pub display_name: String,
    pub transport: String,
    pub last_used_at: Option<String>,
}

// ---------------------------------------------------------------------------
// 检索（阶段 4 扩展，基础结构第一轮已建）
// ---------------------------------------------------------------------------

/// 检索请求（字段与 C# `SearchRequest` 一致；`type` 对应 C# `Type`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub query: String,
    pub scope: Option<MemoryScope>,
    pub project_id: Option<String>,
    #[serde(rename = "type", default)]
    pub memory_type: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub limit: i64,
    #[serde(default)]
    pub semantic_enabled: bool,
}

/// 检索结果 DTO（字段与 C# `SearchResult` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub memory: MemoryItem,
    pub score: f64,
    pub match_reasons: Vec<String>,
}

/// 上下文组装请求（字段与 C# `ContextRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextRequest {
    pub search: SearchRequest,
    pub max_characters: i64,
}

/// 上下文中的单条引用（字段与 C# `ContextEntry` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub id: String,
    pub title: String,
    pub content: String,
    pub match_reasons: Vec<String>,
}

/// 可直接交给 AI 的上下文结果（字段与 C# `ContextResult` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextResult {
    pub items: Vec<ContextEntry>,
    pub character_count: i64,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Embedding（阶段 4）
// ---------------------------------------------------------------------------

/// Embedding 配置视图（本地单机应用：API Key 以明文回显，界面端雾化展示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingSettingsView {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub dimensions: i64,
    pub enabled: bool,
    pub configured: bool,
}

/// Embedding 保存请求（字段与 C# `SaveEmbeddingSettingsRequest` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEmbeddingSettingsRequest {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub dimensions: i64,
    pub enabled: bool,
}

/// 连接测试结果（字段与 C# `EmbeddingTestResult` 一致）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingTestResult {
    pub success: bool,
    pub dimensions: i64,
    pub message: String,
}

/// 向量配置与重建状态（对应 C# `GetStatusAsync` 匿名对象 `{ mode, pendingTasks }`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingStatus {
    pub mode: String,
    pub pending_tasks: i64,
}

/// 全量重建任务票据（对应 C# `QueueRebuildAsync` 匿名对象 `{ id, status }`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildTicket {
    pub id: String,
    pub status: String,
}

// ---------------------------------------------------------------------------
// 记忆图谱（圆形 Obsidian 关系云）
// ---------------------------------------------------------------------------

/// 图谱节点：一条正式记忆的图谱视图（归档记忆与已归档项目中的记忆不进入图谱）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub scope: MemoryScope,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub memory_type: String,
    pub importance: i64,
    pub is_favorite: bool,
    pub is_pinned: bool,
    pub tags: Vec<String>,
    pub keywords: Vec<String>,
    pub updated_at: String,
    /// 该节点在结果集中的关系数（由返回的 edges 统计）。
    pub degree: i64,
}

/// 图谱边：两个记忆 ID 固定排序（a < b）后的无向相似关系。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub memory_id_a: String,
    pub memory_id_b: String,
    pub semantic_score: f64,
    pub keyword_score: f64,
    pub project_boost: f64,
    pub combined_score: f64,
    /// `SEMANTIC` / `KEYWORD` / `MIXED`。
    pub dominant_signal: String,
}

/// 图谱响应：节点、边、中心记忆、截断与构建状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// 局部图谱的中心记忆（全局图谱为 None）。
    pub center_memory_id: Option<String>,
    /// `READY` / `BUILDING`（构建期间仍返回已生成的数据）。
    pub build_status: String,
    /// 节点因 limit 截断时为 true。
    pub truncated: bool,
    /// 满足筛选条件的真实节点总数（截断前）。
    pub total_nodes: i64,
    /// 返回节点集合内的真实关系总数。
    pub total_edges: i64,
}

/// 全局图谱查询：项目集合 + 是否含个人记忆 + 时间范围 + 节点上限 + 分数下限。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphGlobalQuery {
    #[serde(default)]
    pub project_ids: Vec<String>,
    #[serde(default = "default_true")]
    pub include_personal: bool,
    /// `None` = 不限；`Some(7)` / `Some(30)` = 最近 N 天（按 updated_at）。
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default = "default_graph_limit")]
    pub limit: i64,
    #[serde(default = "default_graph_min_score")]
    pub min_score: f64,
}

fn default_true() -> bool {
    true
}

fn default_graph_limit() -> i64 {
    120
}

fn default_graph_min_score() -> f64 {
    0.45
}

impl Default for GraphGlobalQuery {
    fn default() -> Self {
        Self {
            project_ids: Vec::new(),
            include_personal: true,
            days: None,
            limit: default_graph_limit(),
            min_score: default_graph_min_score(),
        }
    }
}

/// 局部图谱查询：中心记忆 + 跳数（1..=2）+ 节点上限 + 分数下限。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNeighborhoodQuery {
    pub memory_id: String,
    #[serde(default = "default_neighborhood_depth")]
    pub depth: i64,
    #[serde(default = "default_graph_limit")]
    pub limit: i64,
    #[serde(default = "default_graph_min_score")]
    pub min_score: f64,
}

fn default_neighborhood_depth() -> i64 {
    2
}

impl Default for GraphNeighborhoodQuery {
    fn default() -> Self {
        Self {
            memory_id: String::new(),
            depth: default_neighborhood_depth(),
            limit: default_graph_limit(),
            min_score: default_graph_min_score(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_keep_csharp_casing() {
        assert_eq!(serde_json::to_string(&MemoryScope::Personal).unwrap(), "\"Personal\"");
        assert_eq!(serde_json::to_string(&MemoryStatus::Archived).unwrap(), "\"Archived\"");
        assert_eq!(
            serde_json::to_string(&McpPermission::ReadWrite).unwrap(),
            "\"ReadWrite\""
        );
        assert_eq!(serde_json::to_string(&McpAssistantType::Codex).unwrap(), "\"Codex\"");
        assert_eq!(serde_json::to_string(&McpClientStatus::Expired).unwrap(), "\"Expired\"");
    }

    /// 反序列化同时接受名称字符串与 C# 默认整数（JsonStringEnumConverter 兼容）。
    #[test]
    fn enums_deserialize_from_string_and_number() {
        assert_eq!(
            serde_json::from_str::<MemoryScope>("\"Project\"").unwrap(),
            MemoryScope::Project
        );
        assert_eq!(serde_json::from_str::<MemoryScope>("1").unwrap(), MemoryScope::Project);
        assert_eq!(serde_json::from_str::<MemoryScope>("0").unwrap(), MemoryScope::Personal);
        assert_eq!(
            serde_json::from_str::<MemoryStatus>("\"Archived\"").unwrap(),
            MemoryStatus::Archived
        );
        assert_eq!(
            serde_json::from_str::<MemoryStatus>("1").unwrap(),
            MemoryStatus::Archived
        );
        assert_eq!(
            serde_json::from_str::<McpPermission>("1").unwrap(),
            McpPermission::ReadWrite
        );
        assert_eq!(
            serde_json::from_str::<McpAssistantType>("3").unwrap(),
            McpAssistantType::Trae
        );
        assert!(serde_json::from_str::<MemoryScope>("\"None\"").is_err());
        assert!(serde_json::from_str::<MemoryScope>("2").is_err());
    }

    /// C# 历史版本快照（无 JsonStringEnumConverter 时写入的整数 scope）必须可读。
    #[test]
    fn snapshot_request_reads_legacy_csharp_integer_scope() {
        let legacy = concat!(
            "{\"scope\":1,\"projectId\":\"0b6c8f5a-1111-4222-8333-444455556666\",",
            "\"title\":\"t\",\"summary\":\"s\",\"content\":\"c\",\"memoryType\":\"Note\",",
            "\"keywords\":[],\"tags\":[],\"importance\":3,\"isFavorite\":false,",
            "\"isPinned\":false,\"cloudProcessingAllowed\":false,\"expectedVersion\":2}"
        );
        let request: SaveMemoryRequest = serde_json::from_str(legacy).unwrap();
        assert_eq!(request.scope, MemoryScope::Project);
        assert_eq!(request.expected_version, Some(2));
        // 序列化回写始终为字符串（与 C# DesktopApiHost JsonStringEnumConverter 一致）。
        assert!(
            serde_json::to_string(&request)
                .unwrap()
                .contains("\"scope\":\"Project\"")
        );
    }

    /// 可空字段必须输出 null（与 C# Web defaults 一致），不得跳过。
    #[test]
    fn memory_item_serializes_null_fields_like_csharp() {
        let memory = MemoryItem {
            id: "0b6c8f5a-1111-4222-8333-444455556666".to_string(),
            scope: MemoryScope::Personal,
            project_id: None,
            project_name: None,
            title: "标题".to_string(),
            summary: String::new(),
            content: "内容".to_string(),
            memory_type: "Note".to_string(),
            keywords: vec![],
            tags: vec![],
            importance: 3,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: false,
            status: MemoryStatus::Active,
            version: 1,
            created_source: "桌面".to_string(),
            updated_source: "桌面".to_string(),
            created_at: "2026-08-15T08:00:00.0000000+00:00".to_string(),
            updated_at: "2026-08-15T08:00:00.0000000+00:00".to_string(),
            archived_at: None,
        };
        let json = serde_json::to_string(&memory).unwrap();
        assert!(json.contains("\"projectId\":null"));
        assert!(json.contains("\"projectName\":null"));
        assert!(json.contains("\"archivedAt\":null"));
        assert!(!json.contains("\"expectedVersion\""));
    }

    #[test]
    fn cursor_page_serializes_camel_case() {
        let page = CursorPage {
            items: vec![1i64, 2, 3],
            next_cursor: Some("YWJj".to_string()),
            has_more: true,
        };
        let json = serde_json::to_value(&page).unwrap();
        assert_eq!(json["items"][0], 1);
        assert_eq!(json["nextCursor"], "YWJj");
        assert_eq!(json["hasMore"], true);
    }

    #[test]
    fn memory_list_query_defaults_size_to_30() {
        let query: MemoryListQuery = serde_json::from_str("{}").unwrap();
        assert_eq!(query.size, 30);
        assert!(query.cursor.is_none());
    }

    #[test]
    fn mcp_client_card_roundtrip() {
        let card = McpClientCard {
            session_id: "11111111-2222-4333-8444-555566667777".to_string(),
            client_key: "codex".to_string(),
            display_name: "Codex".to_string(),
            client_version: None,
            transport: "stdio".to_string(),
            token_prefix: "uam_abcdef123456".to_string(),
            permission: McpPermission::ReadWrite,
            project_id: None,
            project_name: None,
            expires_at: None,
            last_used_at: None,
            call_count: 0,
            created_at: "2026-08-15T08:00:00.0000000+00:00".to_string(),
            revoked_at: None,
            status: McpClientStatus::Active,
        };
        let json = serde_json::to_value(&card).unwrap();
        assert_eq!(json["sessionId"], "11111111-2222-4333-8444-555566667777");
        assert_eq!(json["status"], "Active");
        assert_eq!(json["clientVersion"], serde_json::Value::Null);
        let parsed: McpClientCard = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, card);
    }
}
