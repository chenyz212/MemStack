//! 16 工具分发器：「参数 DTO → memory-application 调用 → structuredContent」映射表。
//!
//! 以 C# `McpTools.cs` 为唯一权威：
//! - 只读工具的范围收口（项目 Token 强制 scope=Project + projectId）。
//! - 写工具先 `require_write`，再按目标记忆/请求做 `require_memory_scope`/`require_project_scope`。
//! - 写入来源严格取 `caller.display_name`（不信任客户端参数）。
//! - 空字符串过滤参数 `EmptyToNull`（IsNullOrWhiteSpace → null，否则 Trim）。
//! - 返回形状与 `contracts/mcp-tools-list.json` 各工具 outputSchema 一致
//!   （集合类工具包 `{"result":[...]}`，单对象直接返回，void 返回 Null）。

use std::sync::Arc;

use memory_application::candidate_service::MemoryCandidateService;
use memory_application::mcp_access::MemoryActivityRecorder;
use memory_application::memory_service::MemoryService;
use memory_application::project_service::ProjectService;
use memory_application::search_service::SearchService;
use memory_application::workspace_service::WorkspaceService;
use memory_domain::{
    BusinessError, ErrorCode, McpCallerContext, MemoryItem, MemoryListQuery, MemoryScope, MemoryStatus,
    SaveMemoryCandidateRequest, SaveMemoryRequest, SaveProjectRequest, SearchRequest, WorkspaceResolution,
};
use serde::Deserialize;
use serde_json::Value;

use crate::permissions::{require_memory_scope, require_project_scope, require_write};
use crate::registry;

/// 分发所需的全部应用服务（由宿主进程装配注入）。
pub struct Services {
    pub memories: Arc<MemoryService>,
    pub candidates: Arc<MemoryCandidateService>,
    pub projects: Arc<ProjectService>,
    pub workspaces: Arc<WorkspaceService>,
    pub search: Arc<SearchService>,
    /// 最近记忆活动记录器（总览活动文案数据源；宿主不注入则跳过记录）。
    pub activity: Option<Arc<MemoryActivityRecorder>>,
    /// 宿主进程工作目录（AI 客户端拉起 MCP 时的项目目录）。
    /// `project_resolve`/`project_create` 缺省工作空间标识时使用，
    /// 使 AI 无需感知自身工作目录即可正确路由项目/个人记忆。
    pub workspace_hint: Option<String>,
}

/// 记录一次成功的记忆读取/创建活动（仅作用域证据存在时；空结果不记录）。
fn record_activity(services: &Services, caller: &McpCallerContext, action: &str, evidence: Option<&str>) {
    let Some(recorder) = &services.activity else {
        return;
    };
    if let Some(scope) = evidence {
        recorder.record(&caller.token_id, action, scope);
    }
}

/// 从结果记忆集合推断作用域证据：Personal / Project / Mixed；无结果返回 None。
fn scope_evidence<'a>(items: impl Iterator<Item = &'a MemoryItem>) -> Option<&'static str> {
    let mut has_personal = false;
    let mut has_project = false;
    for memory in items {
        match memory.scope {
            MemoryScope::Personal => has_personal = true,
            MemoryScope::Project => has_project = true,
        }
    }
    match (has_personal, has_project) {
        (true, true) => Some("Mixed"),
        (true, false) => Some("Personal"),
        (false, true) => Some("Project"),
        (false, false) => None,
    }
}

/// 从上下文入选作用域集合推断证据。
fn scope_evidence_from_set(scopes: &std::collections::BTreeSet<String>) -> Option<&'static str> {
    let has_personal = scopes.iter().any(|scope| scope == "Personal");
    let has_project = scopes.iter().any(|scope| scope == "Project");
    match (has_personal, has_project) {
        (true, true) => Some("Mixed"),
        (true, false) => Some("Personal"),
        (false, true) => Some("Project"),
        (false, false) => None,
    }
}

/// 按工具名执行一次调用，返回 structuredContent 载荷。
///
/// 错误统一为 `BusinessError`，由传输层（stdio/未来 Tauri 侧）转换为
/// 与 C# 对齐的 JSON-RPC 错误形态。
pub fn dispatch(
    name: &str,
    arguments: &Value,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    if !registry::is_known_tool(name) {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            format!("未知工具：{name}"),
        ));
    }
    match name {
        "memory_search" => memory_search(&parse(arguments)?, caller, services),
        "memory_get" => memory_get(&parse(arguments)?, caller, services),
        "memory_recent" => memory_recent(&parse(arguments)?, caller, services),
        "memory_context" => memory_context(&parse(arguments)?, caller, services),
        "memory_related" => memory_related(&parse(arguments)?, caller, services),
        "memory_create" => memory_create(&parse(arguments)?, caller, services),
        "memory_update" => memory_update(&parse(arguments)?, caller, services),
        "memory_archive" => memory_archive(&parse(arguments)?, caller, services),
        "memory_candidate_submit" => candidate_submit(&parse(arguments)?, caller, services),
        "memory_candidate_list" => candidate_list(caller, services),
        "memory_candidate_confirm" => candidate_confirm(&parse(arguments)?, caller, services),
        "memory_candidate_reject" => candidate_reject(&parse(arguments)?, caller, services),
        "project_list" => project_list(caller, services),
        "project_resolve" => project_resolve(&parse(arguments)?, caller, services),
        "project_create" => project_create(&parse(arguments)?, caller, services),
        "project_update" => project_update(&parse(arguments)?, caller, services),
        _ => unreachable!("registry 已校验，快照与 match 分支必须同步维护"),
    }
}

/// 参数反序列化：缺字段/类型不符 → 参数错误（错误形态由传输层对齐 C#）。
fn parse<T: serde::de::DeserializeOwned>(arguments: &Value) -> Result<T, BusinessError> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("参数解析失败：{error}")))
}

/// C# `EmptyToNull`：IsNullOrWhiteSpace → None，否则 Some(trim)。
fn empty_to_null(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.trim().to_string())
    }
}

/// 按调用方身份收口检索范围（项目 Token 强制绑定项目，对应 C# SearchAsync/RecentAsync）。
fn scoped_request(caller: &McpCallerContext) -> (Option<MemoryScope>, Option<String>) {
    if caller.project_id.is_some() {
        (Some(MemoryScope::Project), caller.project_id.clone())
    } else {
        (None, None)
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, BusinessError> {
    serde_json::to_value(value)
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("序列化结果失败：{error}")))
}

// ---------------------------------------------------------------------------
// 只读：检索
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchArgs {
    query: String,
    memory_type: String,
    tag: String,
    limit: i64,
    semantic_enabled: bool,
}

fn memory_search(args: &SearchArgs, caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    let (scope, project_id) = scoped_request(caller);
    let request = SearchRequest {
        query: args.query.clone(),
        scope,
        project_id,
        memory_type: empty_to_null(&args.memory_type),
        tag: empty_to_null(&args.tag),
        limit: args.limit,
        semantic_enabled: args.semantic_enabled,
    };
    let results = services.search.search(&request)?;
    record_activity(
        services,
        caller,
        "READ",
        scope_evidence(results.iter().map(|result| &result.memory)),
    );
    to_value(&serde_json::json!({ "result": results }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemoryIdArgs {
    memory_id: String,
}

fn memory_get(args: &MemoryIdArgs, caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    let memory = services.memories.get(&args.memory_id)?;
    require_memory_scope(caller, &memory)?;
    record_activity(services, caller, "READ", scope_evidence(std::iter::once(&memory)));
    to_value(&memory)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentArgs {
    limit: i64,
}

fn memory_recent(args: &RecentArgs, caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    let (scope, project_id) = scoped_request(caller);
    let query = MemoryListQuery {
        scope,
        project_id,
        status: Some(MemoryStatus::Active),
        is_favorite: None,
        is_pinned: None,
        memory_type: None,
        tag: None,
        importance_min: None,
        cursor: None,
        size: args.limit,
    };
    let page = services.memories.list(&query)?;
    record_activity(services, caller, "READ", scope_evidence(page.items.iter()));
    to_value(&serde_json::json!({ "result": page.items }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextArgs {
    query: String,
    max_characters: i64,
    limit: i64,
    semantic_enabled: bool,
}

fn memory_context(args: &ContextArgs, caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    let (scope, project_id) = scoped_request(caller);
    let request = memory_domain::ContextRequest {
        search: SearchRequest {
            query: args.query.clone(),
            scope,
            project_id,
            memory_type: None,
            tag: None,
            limit: args.limit,
            semantic_enabled: args.semantic_enabled,
        },
        max_characters: args.max_characters,
    };
    let (context, scopes) = services.search.build_context_detailed(&request)?;
    record_activity(services, caller, "READ", scope_evidence_from_set(&scopes));
    to_value(&context)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelatedArgs {
    memory_id: String,
    limit: i64,
}

fn memory_related(args: &RelatedArgs, caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    // 与 C# RelatedAsync 一致：先按记忆范围守卫读取基准，关键词优先、无关键词用标题。
    let memory = services.memories.get(&args.memory_id)?;
    require_memory_scope(caller, &memory)?;
    let query = if memory.keywords.is_empty() {
        memory.title.clone()
    } else {
        memory.keywords.join(" ")
    };
    let (scope, project_id) = scoped_request(caller);
    let request = SearchRequest {
        query,
        scope,
        project_id,
        memory_type: None,
        tag: None,
        limit: args.limit + 1,
        semantic_enabled: true,
    };
    let results = services
        .search
        .search(&request)?
        .into_iter()
        .filter(|item| item.memory.id != args.memory_id)
        .take(args.limit.max(0) as usize)
        .collect::<Vec<_>>();
    record_activity(
        services,
        caller,
        "READ",
        scope_evidence(results.iter().map(|result| &result.memory)),
    );
    to_value(&serde_json::json!({ "result": results }))
}

// ---------------------------------------------------------------------------
// 写：正式记忆
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateMemoryArgs {
    scope: MemoryScope,
    project_id: Option<String>,
    title: String,
    summary: String,
    content: String,
    memory_type: String,
    keywords: Vec<String>,
    tags: Vec<String>,
    importance: i64,
    cloud_processing_allowed: bool,
}

fn memory_create(
    args: &CreateMemoryArgs,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    require_write(caller)?;
    require_project_scope(caller, args.scope, args.project_id.as_deref())?;
    let request = SaveMemoryRequest {
        scope: args.scope,
        project_id: args.project_id.clone(),
        title: args.title.clone(),
        summary: args.summary.clone(),
        content: args.content.clone(),
        memory_type: args.memory_type.clone(),
        keywords: args.keywords.clone(),
        tags: args.tags.clone(),
        importance: args.importance,
        is_favorite: false,
        is_pinned: false,
        cloud_processing_allowed: args.cloud_processing_allowed,
        expected_version: None,
    };
    let memory = services.memories.create_from_source(&request, &caller.display_name)?;
    record_activity(services, caller, "CREATE", scope_evidence(std::iter::once(&memory)));
    to_value(&memory)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateMemoryArgs {
    memory_id: String,
    scope: MemoryScope,
    project_id: Option<String>,
    title: String,
    summary: String,
    content: String,
    memory_type: String,
    keywords: Vec<String>,
    tags: Vec<String>,
    importance: i64,
    is_favorite: bool,
    is_pinned: bool,
    cloud_processing_allowed: bool,
    expected_version: i64,
}

fn memory_update(
    args: &UpdateMemoryArgs,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    require_write(caller)?;
    let memory = services.memories.get(&args.memory_id)?;
    require_memory_scope(caller, &memory)?;
    require_project_scope(caller, args.scope, args.project_id.as_deref())?;
    let request = SaveMemoryRequest {
        scope: args.scope,
        project_id: args.project_id.clone(),
        title: args.title.clone(),
        summary: args.summary.clone(),
        content: args.content.clone(),
        memory_type: args.memory_type.clone(),
        keywords: args.keywords.clone(),
        tags: args.tags.clone(),
        importance: args.importance,
        is_favorite: args.is_favorite,
        is_pinned: args.is_pinned,
        cloud_processing_allowed: args.cloud_processing_allowed,
        expected_version: Some(args.expected_version),
    };
    to_value(
        &services
            .memories
            .update_from_source(&args.memory_id, &request, &caller.display_name)?,
    )
}

fn memory_archive(args: &MemoryIdArgs, caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    require_write(caller)?;
    let memory = services.memories.get(&args.memory_id)?;
    require_memory_scope(caller, &memory)?;
    to_value(
        &services
            .memories
            .archive_from_source(&args.memory_id, &caller.display_name)?,
    )
}

// ---------------------------------------------------------------------------
// 候选记忆
// ---------------------------------------------------------------------------

fn candidate_submit(
    args: &CreateMemoryArgs,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    require_write(caller)?;
    require_project_scope(caller, args.scope, args.project_id.as_deref())?;
    let request = SaveMemoryCandidateRequest {
        scope: args.scope,
        project_id: args.project_id.clone(),
        title: args.title.clone(),
        summary: args.summary.clone(),
        content: args.content.clone(),
        memory_type: args.memory_type.clone(),
        keywords: args.keywords.clone(),
        tags: args.tags.clone(),
        importance: args.importance,
        cloud_processing_allowed: args.cloud_processing_allowed,
        expected_version: None,
    };
    to_value(&services.candidates.submit(&request, &caller.display_name)?)
}

fn candidate_list(caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    let candidates = services.candidates.list(caller)?;
    to_value(&serde_json::json!({ "result": candidates }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CandidateDecisionArgs {
    candidate_id: String,
    expected_version: i64,
}

fn candidate_confirm(
    args: &CandidateDecisionArgs,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    // 服务内部完成 require_write + require_project_scope + 乐观锁（与 C# ConfirmAsync 一致）。
    to_value(
        &services
            .candidates
            .confirm(&args.candidate_id, args.expected_version, caller)?,
    )
}

fn candidate_reject(
    args: &CandidateDecisionArgs,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    services
        .candidates
        .reject(&args.candidate_id, args.expected_version, caller)?;
    // C# 工具返回 Task（void）→ 无 structuredContent。
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// 项目
// ---------------------------------------------------------------------------

fn project_list(caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    let projects = services.projects.list(false)?;
    let visible = match &caller.project_id {
        Some(bound) => projects
            .into_iter()
            .filter(|project| &project.id == bound)
            .collect::<Vec<_>>(),
        None => projects,
    };
    to_value(&serde_json::json!({ "result": visible }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveArgs {
    /// 可选：缺省使用宿主工作目录（AI 客户端无法可靠感知自身路径）。
    workspace_identifier: Option<String>,
}

/// 判定 workspace_hint 是否形如真实文件系统路径。
pub fn is_path_like_hint(hint: &str) -> bool {
    let trimmed = hint.trim();
    trimmed.contains(":\\") || trimmed.starts_with("\\\\") || trimmed.starts_with('/')
}

/// 判定 workspace_hint 是否为宿主运行时目录（不是用户工作空间）：
/// - 用户主目录：TraeWork 等宿主以 `C:\Users\<name>` 为 cwd 拉起；
/// - MCP 连接实例目录：WorkBuddy 以 `…\custom-mcp_<server>-<hash>` 为 cwd
///   （绝对路径形态，含 `:\`，仅靠 is_path_like_hint 挡不住）。
///
/// 两者出现在 hint 中时视为无工作空间上下文（UNBOUND / 拒绝创建）。
pub fn is_runtime_dir_hint(hint: &str) -> bool {
    let trimmed = hint.trim().trim_end_matches(['\\', '/']);
    if let Some(profile) = std::env::var_os("USERPROFILE")
        && !profile.is_empty()
    {
        let profile_text = profile.to_string_lossy();
        let profile = profile_text.as_ref().trim().trim_end_matches(['\\', '/']);
        if !profile.is_empty() && profile.eq_ignore_ascii_case(trimmed) {
            return true;
        }
    }
    trimmed
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .starts_with("custom-mcp_")
}

/// 从 services.workspace_hint 提取可用的工作空间标识：
/// 显式参数优先；hint 仅采纳「形如真实路径且非宿主运行时目录」的值。
fn usable_hint(services: &Services) -> Option<String> {
    services
        .workspace_hint
        .clone()
        .filter(|hint| is_path_like_hint(hint) && !is_runtime_dir_hint(hint))
}

fn project_resolve(args: &ResolveArgs, caller: &McpCallerContext, services: &Services) -> Result<Value, BusinessError> {
    // 缺省工作空间标识 = 宿主工作目录（仅当可用时采用）；显式空串视为未传。
    let identifier = args
        .workspace_identifier
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| usable_hint(services));
    // 无可用标识（AI 客户端 cwd 是连接实例目录等，纯对话场景）：返回安全 UNBOUND，
    // AI 一律存个人记忆且不询问项目名，流程不中断。
    let Some(identifier) = identifier else {
        return to_value(&WorkspaceResolution {
            status: "UNBOUND".to_string(),
            workspace_identifier: String::new(),
            project: None,
            requires_user_input: false,
            question: None,
        });
    };
    let mut resolution = services.workspaces.resolve(&identifier)?;
    if let (Some(project), Some(bound)) = (&resolution.project, &caller.project_id)
        && &project.id != bound
    {
        return Err(BusinessError::with_message(
            ErrorCode::McpProjectScopeDenied,
            "当前 Token 不能访问该工作空间项目",
        ));
    }
    // 工作空间存在但未绑定项目（PROJECT_NAME_REQUIRED）：保留询问文案，
    // 指示 AI 向用户询问一次中文项目名，再 project_create(中文名, 该标识) 完成绑定。
    // 与"无工作空间"的 UNBOUND 区分开：前者进项目空间，后者存个人。
    resolution.requires_user_input = resolution.status == "PROJECT_NAME_REQUIRED";
    to_value(&resolution)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveProjectArgs {
    name: String,
    description: String,
    color: String,
    /// 可选：创建后自动绑定的工作空间标识（缺省使用宿主工作目录）。
    workspace_identifier: Option<String>,
}

fn project_create(
    args: &SaveProjectArgs,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    require_write(caller)?;
    if caller.project_id.is_some() {
        return Err(BusinessError::with_message(
            ErrorCode::McpProjectScopeDenied,
            "项目范围 Token 不能创建其他项目",
        ));
    }
    // 工作空间标识必填语义：显式空串视为未传（AI 客户端常见），回退宿主工作目录
    // （仅当可用：形如真实路径且非宿主运行时目录）；
    // 两者皆无 → 拒绝创建，杜绝落库无绑定标识的项目。
    let identifier = args
        .workspace_identifier
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| usable_hint(services))
        .ok_or_else(|| {
            BusinessError::with_message(
                ErrorCode::WorkspaceIdentifierInvalid,
                "无法确定工作空间标识：请显式传入 workspaceIdentifier（用户当前工作空间根目录的完整路径或目录名）",
            )
        })?;
    // 前置校验（标识合法 + 未被活动项目占用）放在创建之前：失败则项目不落库。
    services.workspaces.ensure_bind_target(&identifier)?;
    let project = services.projects.create(&SaveProjectRequest {
        name: args.name.clone(),
        description: args.description.clone(),
        color: args.color.clone(),
    })?;
    // 绑定失败（理论上仅剩并发竞态）如实报错，不再静默返回未绑定项目。
    let bound = services.workspaces.bind_workspace(&project.id, &identifier)?;
    to_value(&bound)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProjectArgs {
    project_id: String,
    name: String,
    description: String,
    color: String,
}

fn project_update(
    args: &UpdateProjectArgs,
    caller: &McpCallerContext,
    services: &Services,
) -> Result<Value, BusinessError> {
    require_write(caller)?;
    if let Some(bound) = &caller.project_id
        && bound != &args.project_id
    {
        return Err(BusinessError::with_message(
            ErrorCode::McpProjectScopeDenied,
            "当前 Token 不能修改该项目",
        ));
    }
    to_value(&services.projects.update(
        &args.project_id,
        &SaveProjectRequest {
            name: args.name.clone(),
            description: args.description.clone(),
            color: args.color.clone(),
        },
    )?)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use memory_application::search_service::QueryVectorProvider;
    use memory_application::{Database, GuidGenerator, SystemClock};
    use memory_domain::{McpCallerContext, McpPermission};
    use serde_json::json;

    use super::*;

    /// keyword 模式查询向量提供方（不依赖 Embedding 配置）。
    struct NoVectors;
    impl QueryVectorProvider for NoVectors {
        fn try_create(&self, _query: &str) -> Option<Vec<f32>> {
            None
        }
    }

    struct TestContext {
        #[allow(dead_code)]
        directory: tempfile::TempDir,
        database: Database,
        services: Services,
        owner: McpCallerContext,
    }

    fn context() -> TestContext {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dispatch.db");
        drop(memory_storage::open_initialized(&path).unwrap());
        let database = Database::new(path);
        let clock = Arc::new(SystemClock);
        let ids = Arc::new(GuidGenerator);
        let memories = Arc::new(MemoryService::new(database.clone(), clock.clone(), ids.clone()));
        let projects = Arc::new(ProjectService::new(database.clone(), clock.clone(), ids.clone()));
        let workspaces = Arc::new(WorkspaceService::new(
            database.clone(),
            projects.clone(),
            memories.clone(),
            clock.clone(),
        ));
        let candidates = Arc::new(MemoryCandidateService::new(
            database.clone(),
            memories.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let search = Arc::new(SearchService::new(database.clone(), Arc::new(NoVectors)));
        let activity = Arc::new(memory_application::mcp_access::MemoryActivityRecorder::new(
            database.clone(),
            clock.clone(),
        ));
        TestContext {
            directory: temp,
            database,
            services: Services {
                memories,
                candidates,
                projects,
                workspaces,
                search,
                activity: Some(activity),
                workspace_hint: Some(r"E:\ws\hint".to_string()),
            },
            owner: McpCallerContext {
                token_id: "00000000-0000-4000-8000-000000000001".to_string(),
                display_name: "分发测试客户端".to_string(),
                permission: McpPermission::ReadWrite,
                project_id: None,
            },
        }
    }

    fn caller(permission: McpPermission, project_id: Option<&str>) -> McpCallerContext {
        McpCallerContext {
            token_id: "00000000-0000-4000-8000-000000000002".to_string(),
            display_name: "受限客户端".to_string(),
            permission,
            project_id: project_id.map(str::to_string),
        }
    }

    fn create_args(content: &str) -> Value {
        json!({
            "scope": "Personal",
            "projectId": null,
            "title": format!("标题-{content}"),
            "summary": "",
            "content": content,
            "memoryType": "NOTE",
            "keywords": ["检索词"],
            "tags": [],
            "importance": 3,
            "cloudProcessingAllowed": false,
        })
    }

    #[test]
    fn readonly_tools_return_contract_shapes() {
        let context = context();
        let created = dispatch(
            "memory_create",
            &create_args("分发器只读路径验证正文"),
            &context.owner,
            &context.services,
        )
        .unwrap();
        let memory_id = created["id"].as_str().unwrap().to_string();

        let search = dispatch(
            "memory_search",
            &json!({"query":"检索词","memoryType":"","tag":"","limit":10,"semanticEnabled":false}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(search["result"].as_array().unwrap().len(), 1);

        let get = dispatch(
            "memory_get",
            &json!({"memoryId": memory_id}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(get["id"].as_str(), Some(memory_id.as_str()));
        assert!(get["structured"].is_null());

        let recent = dispatch("memory_recent", &json!({"limit": 5}), &context.owner, &context.services).unwrap();
        assert_eq!(recent["result"].as_array().unwrap().len(), 1);

        let context_result = dispatch(
            "memory_context",
            &json!({"query":"检索词","maxCharacters":200,"limit":10,"semanticEnabled":false}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(context_result["truncated"], json!(false));
        assert!(!context_result["items"].as_array().unwrap().is_empty());

        // related：排除自身。
        let related = dispatch(
            "memory_related",
            &json!({"memoryId": memory_id, "limit": 5}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(related["result"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn write_tools_enforce_permissions_and_scope() {
        let context = context();
        let read_only = caller(McpPermission::Read, None);
        let error = dispatch("memory_create", &create_args("只读拒写"), &read_only, &context.services).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpPermissionDenied);

        // 项目 Token 创建个人记忆 → 拒绝。
        let bound = caller(McpPermission::ReadWrite, Some("00000000-0000-4000-8000-000000000009"));
        let error = dispatch("memory_create", &create_args("越权正文"), &bound, &context.services).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpProjectScopeDenied);

        // 项目 Token 读取他人个人记忆 → 拒绝。
        let created = dispatch(
            "memory_create",
            &create_args("守卫样本"),
            &context.owner,
            &context.services,
        )
        .unwrap();
        let memory_id = created["id"].as_str().unwrap().to_string();
        let error = dispatch("memory_get", &json!({"memoryId": memory_id}), &bound, &context.services).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpProjectScopeDenied);

        // 项目 Token 创建项目 → 拒绝。
        let error = dispatch(
            "project_create",
            &json!({"name":"越权项目","description":"","color":"#111111"}),
            &bound,
            &context.services,
        )
        .unwrap_err();
        assert_eq!(error.message, "项目范围 Token 不能创建其他项目");
    }

    #[test]
    fn candidate_flow_and_project_tools() {
        let context = context();
        // 候选：提交 → 列表 → 确认。
        let submitted = dispatch(
            "memory_candidate_submit",
            &create_args("候选正文"),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(submitted["sourceName"], json!("分发测试客户端"));
        let candidate_id = submitted["id"].as_str().unwrap().to_string();
        let listed = dispatch("memory_candidate_list", &json!({}), &context.owner, &context.services).unwrap();
        assert_eq!(listed["result"].as_array().unwrap().len(), 1);
        let confirmed = dispatch(
            "memory_candidate_confirm",
            &json!({"candidateId": candidate_id, "expectedVersion": 1}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(confirmed["status"], json!("Active"));
        // 候选清空。
        let listed = dispatch("memory_candidate_list", &json!({}), &context.owner, &context.services).unwrap();
        assert_eq!(listed["result"].as_array().unwrap().len(), 0);

        // 项目：创建（缺省自动绑定宿主工作目录 hint）→ 列表 → 修改 → 解析。
        let project = dispatch(
            "project_create",
            &json!({"name":"分发项目","description":"描述","color":"#238f7a"}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        let project_id = project["id"].as_str().unwrap().to_string();
        assert_eq!(project["workspaceBound"], json!(true), "缺省绑定宿主工作目录");
        assert_eq!(project["workspaceIdentifier"], json!("hint"));
        let listed = dispatch("project_list", &json!({}), &context.owner, &context.services).unwrap();
        assert_eq!(listed["result"].as_array().unwrap().len(), 1);
        let updated = dispatch(
            "project_update",
            &json!({"projectId": project_id, "name":"分发项目二","description":"描述","color":"#238f7a"}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(updated["name"], json!("分发项目二"));
        // 缺省参数 resolve → 使用 hint → MAPPED 到刚绑定的项目。
        let resolution = dispatch("project_resolve", &json!({}), &context.owner, &context.services).unwrap();
        assert_eq!(resolution["status"], json!("MAPPED"));
        assert_eq!(resolution["project"]["id"], json!(project_id));
        // 显式未绑定标识 → PROJECT_NAME_REQUIRED：携带询问中文项目名的文案，
        // AI 询问用户后 project_create(中文名, 该标识) 完成首次绑定闭环。
        let resolution = dispatch(
            "project_resolve",
            &json!({"workspaceIdentifier": "e:\\AICoding\\nonexistent-workspace"}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(resolution["status"], json!("PROJECT_NAME_REQUIRED"));
        assert!(resolution["project"].is_null());
        assert!(
            resolution["question"]
                .as_str()
                .is_some_and(|text| text.contains("中文项目名称"))
        );
        assert_eq!(resolution["requiresUserInput"], json!(true));

        // 拒绝路径：提交候选后 reject 返回 Null 且清空。
        let submitted = dispatch(
            "memory_candidate_submit",
            &create_args("拒绝候选"),
            &context.owner,
            &context.services,
        )
        .unwrap();
        let rejected = dispatch(
            "memory_candidate_reject",
            &json!({"candidateId": submitted["id"].as_str().unwrap(), "expectedVersion": 1}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert!(rejected.is_null());
    }

    #[test]
    fn update_archive_follow_optimistic_lock() {
        let context = context();
        let created = dispatch(
            "memory_create",
            &create_args("版本控制正文"),
            &context.owner,
            &context.services,
        )
        .unwrap();
        let memory_id = created["id"].as_str().unwrap().to_string();

        let mut update_args = create_args("版本控制正文改");
        update_args["memoryId"] = json!(memory_id);
        update_args["isFavorite"] = json!(true);
        update_args["isPinned"] = json!(false);
        update_args["expectedVersion"] = json!(1);
        let updated = dispatch("memory_update", &update_args, &context.owner, &context.services).unwrap();
        assert_eq!(updated["version"], json!(2));
        assert_eq!(updated["updatedSource"], json!("分发测试客户端"));

        // 过期版本 → 冲突。
        let error = dispatch("memory_update", &update_args, &context.owner, &context.services).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryVersionConflict);

        let archived = dispatch(
            "memory_archive",
            &json!({"memoryId": memory_id}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(archived["status"], json!("Archived"));
    }

    #[test]
    fn unknown_tool_and_bad_arguments_are_rejected() {
        let context = context();
        let error = dispatch("memory_delete", &json!({}), &context.owner, &context.services).unwrap_err();
        assert!(error.message.contains("未知工具"));

        let error = dispatch(
            "memory_search",
            &json!({"query": "缺失参数"}),
            &context.owner,
            &context.services,
        )
        .unwrap_err();
        assert!(error.message.contains("参数解析失败"));
    }

    #[test]
    fn non_path_hint_is_ignored() {
        // WorkBuddy/TraeWork 等客户端以连接实例目录为 cwd 拉起：
        // 该值绝不能作为工作空间标识（绑定出怪值 / 解析永远 UNBOUND）。
        assert!(!is_path_like_hint("custom-mcp_unifiedAiMemory-79b438af"));
        assert!(is_path_like_hint(r"E:\ws\DifySys"));
        assert!(is_path_like_hint("\\\\server\\share"));
        assert!(is_path_like_hint("/home/user/project"));

        let mut context = context();
        context.services.workspace_hint = Some("custom-mcp_unifiedAiMemory-79b438af".to_string());
        // resolve 无参：不采用实例目录 → 安全 UNBOUND（AI 存个人记忆，流程不中断）。
        let resolution = dispatch("project_resolve", &json!({}), &context.owner, &context.services).unwrap();
        assert_eq!(resolution["status"], json!("UNBOUND"));
        assert_eq!(resolution["requiresUserInput"], json!(false));
        // create 无参：拒绝创建，错误引导 AI 显式传入用户工作空间。
        let error = dispatch(
            "project_create",
            &json!({"name":"项目","description":"描述","color":"#238f7a"}),
            &context.owner,
            &context.services,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::WorkspaceIdentifierInvalid);
        assert!(error.message.contains("workspaceIdentifier"));
        // 显式传标识不受实例目录影响。
        let project = dispatch(
            "project_create",
            &json!({
                "name":"显式项目","description":"描述","color":"#238f7a",
                "workspaceIdentifier":"E:\\ws\\DifySys"
            }),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(project["workspaceBound"], json!(true));
        assert_eq!(project["workspaceIdentifier"], json!("DifySys"));
    }

    #[test]
    fn runtime_dir_hint_is_ignored() {
        // 宿主运行时目录形态（实测日志捕获）：
        // - TraeWork 以用户主目录为 cwd（C:\Users\<name>，形如真实路径但非工作空间）；
        // - WorkBuddy 的连接实例目录为绝对路径（…\custom-mcp_<server>-<hash>，
        //   含 :\，仅靠 is_path_like_hint 挡不住）。
        let workbuddy_cwd = r"C:\Users\z\.workbuddy\logs\mcp-runtime\custom-mcp_unifiedAiMemory-79b438af";
        assert!(is_path_like_hint(workbuddy_cwd), "WorkBuddy 实例目录是绝对路径");
        assert!(is_runtime_dir_hint(workbuddy_cwd));
        assert!(is_runtime_dir_hint("custom-mcp_unifiedAiMemory-79b438af"));
        if let Ok(profile) = std::env::var("USERPROFILE") {
            assert!(is_runtime_dir_hint(&profile), "用户主目录 {profile} 应被过滤");
        }
        assert!(!is_runtime_dir_hint(r"E:\ws\DifySys"));
        // 前缀精确匹配 `custom-mcp_`（带下划线），普通目录名不受影响。
        assert!(!is_runtime_dir_hint(r"E:\ws\custom-mcp-helper"));

        let mut context = context();
        if let Ok(profile) = std::env::var("USERPROFILE") {
            context.services.workspace_hint = Some(profile);
            // 纯对话（cwd=主目录）→ UNBOUND，不询问项目名。
            let resolution = dispatch("project_resolve", &json!({}), &context.owner, &context.services).unwrap();
            assert_eq!(resolution["status"], json!("UNBOUND"), "主目录 hint 不得触发询问");
            // create 无参 → 拒绝。
            let error = dispatch(
                "project_create",
                &json!({"name":"项目","description":"描述","color":"#238f7a"}),
                &context.owner,
                &context.services,
            )
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::WorkspaceIdentifierInvalid);
        }
        // WorkBuddy 绝对路径实例目录 → 同样 UNBOUND / 拒绝。
        context.services.workspace_hint = Some(workbuddy_cwd.to_string());
        let resolution = dispatch("project_resolve", &json!({}), &context.owner, &context.services).unwrap();
        assert_eq!(resolution["status"], json!("UNBOUND"), "实例目录 hint 不得触发询问");
        let error = dispatch(
            "project_create",
            &json!({"name":"项目","description":"描述","color":"#238f7a"}),
            &context.owner,
            &context.services,
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::WorkspaceIdentifierInvalid);
    }

    #[test]
    fn resolve_without_any_identifier_returns_unbound() {
        let mut context = context();
        context.services.workspace_hint = None;
        let resolution = dispatch("project_resolve", &json!({}), &context.owner, &context.services).unwrap();
        assert_eq!(resolution["status"], json!("UNBOUND"));
        assert_eq!(resolution["requiresUserInput"], json!(false));
    }

    #[test]
    fn memory_activity_recorded_for_successful_reads_and_creates_only() {
        let context = context();
        // 为 owner 建立真实 Token 行，活动才有落点。
        context
            .database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO mcp_token(id,name,display_name,token_hash,access_mode,project_scope_json,created_at) \
                 VALUES('00000000-0000-4000-8000-000000000001','测试','分发测试客户端','hash','ReadWrite','[]',\
                 '2026-08-16T08:00:00.0000000+00:00');",
                [],
            )
            .unwrap();
        let created = dispatch(
            "memory_create",
            &create_args("活动记录正文"),
            &context.owner,
            &context.services,
        )
        .unwrap();
        let memory_id = created["id"].as_str().unwrap().to_string();
        let read_activity = |context: &TestContext| {
            context
                .database
                .open()
                .unwrap()
                .query_row(
                    "SELECT last_memory_action,last_action_scope FROM mcp_token \
                     WHERE id='00000000-0000-4000-8000-000000000001';",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap()
        };
        // 创建成功 → CREATE / Personal。
        assert_eq!(read_activity(&context), ("CREATE".to_string(), "Personal".to_string()));
        // 读取成功 → READ / Personal。
        dispatch(
            "memory_get",
            &json!({"memoryId": memory_id}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(read_activity(&context), ("READ".to_string(), "Personal".to_string()));
        // 空结果检索不覆盖活动（无作用域证据）。
        dispatch(
            "memory_search",
            &json!({"query":"不存在的检索词xyz","memoryType":"","tag":"","limit":10,"semanticEnabled":false}),
            &context.owner,
            &context.services,
        )
        .unwrap();
        assert_eq!(read_activity(&context), ("READ".to_string(), "Personal".to_string()));
        // 失败调用不记录：memory_get 不存在的记忆报错，活动保持不变。
        let error = dispatch(
            "memory_get",
            &json!({"memoryId": "not-exist-id"}),
            &context.owner,
            &context.services,
        );
        assert!(error.is_err());
        assert_eq!(read_activity(&context), ("READ".to_string(), "Personal".to_string()));
        // 权限拒绝不记录：只读 Token 创建失败。
        let read_only = caller(McpPermission::Read, None);
        let denied = dispatch("memory_create", &create_args("拒绝正文"), &read_only, &context.services);
        assert!(denied.is_err());
        assert_eq!(read_activity(&context), ("READ".to_string(), "Personal".to_string()));
    }
}
