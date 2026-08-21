//! 工作空间服务：移植 C# `WorkspaceService`（工作空间与项目的一对一绑定）。
//!
//! 语义对齐要点：
//! - resolve：未绑定返回 `PROJECT_NAME_REQUIRED` + 中文询问；已绑定项目归档抛错。
//! - bind：项目不存在/已归档/已绑其他工作空间分别抛 `PROJECT_NOT_FOUND` /
//!   `PROJECT_ARCHIVED` / `PROJECT_WORKSPACE_OCCUPIED`；唯一索引兜底 `WORKSPACE_ALREADY_BOUND`。
//! - store_memory：首次无项目名只询问；有项目名时进程内互斥串行「查找或创建 + 绑定」，
//!   再委托 `MemoryService::create` 写入 Project 记忆。

use std::sync::{Arc, Mutex};

use memory_domain::{
    BusinessError, ErrorCode, MemoryScope, ProjectItem, SaveProjectRequest, WorkspaceMemoryRequest,
    WorkspaceMemoryResult, WorkspaceResolution,
};
use rusqlite::{Connection, params};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::memory_service::MemoryService;
use crate::project_service::ProjectService;
use crate::sqlite_errors::{map_sqlite_error, unique_constraint_error};
use crate::workspace_identity::{calculate_key, normalize_identifier};

/// 与 C# 一致的默认项目颜色。
const DEFAULT_PROJECT_COLOR: &str = "#238f7a";

/// 首次绑定提问文案（与 C# 逐字一致）。
const FIRST_BINDING_QUESTION: &str = "这是第一次在该工作空间保存记忆，请告诉我它对应的中文项目名称。";

/// 管理工作空间与中文项目之间的一对一绑定。
pub struct WorkspaceService {
    database: Database,
    projects: Arc<ProjectService>,
    memories: Arc<MemoryService>,
    clock: Arc<dyn Clock>,
    /// 进程内首次绑定串行化（C# `SemaphoreSlim(1,1)` 的等价物）。
    first_binding_lock: Mutex<()>,
}

impl WorkspaceService {
    /// 创建工作空间应用服务。
    pub fn new(
        database: Database,
        projects: Arc<ProjectService>,
        memories: Arc<MemoryService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            database,
            projects,
            memories,
            clock,
            first_binding_lock: Mutex::new(()),
        }
    }

    /// 查询工作空间当前绑定，未绑定时返回需要用户命名的结构化结果。
    pub fn resolve(&self, workspace_identifier: &str) -> Result<WorkspaceResolution, BusinessError> {
        let workspace = create_workspace_value(workspace_identifier)?;
        let project = self.find_by_workspace_key(&workspace.key)?;
        let Some(project) = project else {
            return Ok(WorkspaceResolution {
                status: "PROJECT_NAME_REQUIRED".to_string(),
                workspace_identifier: workspace.identifier,
                project: None,
                requires_user_input: true,
                question: Some(FIRST_BINDING_QUESTION.to_string()),
            });
        };
        ensure_project_active(&project)?;
        self.touch(&project.id)?;
        Ok(WorkspaceResolution {
            status: "MAPPED".to_string(),
            workspace_identifier: workspace.identifier,
            project: Some(project),
            requires_user_input: false,
            question: None,
        })
    }

    /// 将指定活动项目绑定到工作空间。
    ///
    /// 绑定规则（编辑换绑语义）：
    /// - 项目自身已绑定其他标识 → 允许直接换绑（UPDATE 覆盖）。
    /// - 目标标识被其他活动项目占用 → `WORKSPACE_ALREADY_BOUND`（消息含占用项目名）。
    /// - 目标标识仅被已归档项目占用 → 抢占：清除归档项目绑定后绑定本项目。
    pub fn bind_workspace(&self, project_id: &str, workspace_identifier: &str) -> Result<ProjectItem, BusinessError> {
        let workspace = create_workspace_value(workspace_identifier)?;
        let now_text = format_storage_time(self.clock.now_utc());
        let mut connection = self.database.open()?;
        ensure_project_can_bind(&connection, project_id)?;
        // 抢占归档占用者 + 绑定本项目必须原子（单事务），避免中间态。
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        // 目标占用检查（排除自己）：活动项目占用报错；归档项目占用则抢占。
        let occupant: Option<(String, i64, String)> = transaction
            .query_row(
                "SELECT id, is_archived, name FROM project WHERE workspace_key=$key;",
                params![workspace.key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        if let Some((occupant_id, archived, name)) = occupant
            && occupant_id != project_id
        {
            if archived == 0 {
                return Err(BusinessError::with_message(
                    ErrorCode::WorkspaceAlreadyBound,
                    format!("该工作空间已绑定项目「{name}」，请先解绑或更换标识"),
                ));
            }
            transaction
                .execute(
                    "UPDATE project SET workspace_key=NULL, workspace_uri=NULL, workspace_label=NULL, \
                     workspace_last_seen_at=NULL, updated_at=$updated_at WHERE id=$id;",
                    params![now_text, occupant_id],
                )
                .map_err(map_sqlite_error)?;
        }
        let result = transaction.execute(
            "UPDATE project SET workspace_key=$workspace_key, workspace_uri=NULL, \
             workspace_label=$workspace_identifier, workspace_last_seen_at=$seen_at, updated_at=$updated_at \
             WHERE id=$id AND is_archived=0;",
            params![workspace.key, workspace.identifier, now_text, now_text, project_id],
        );
        match result {
            Ok(_) => {}
            Err(error) => {
                return if unique_constraint_error(&error).is_some() {
                    Err(BusinessError::new(ErrorCode::WorkspaceAlreadyBound))
                } else {
                    Err(map_sqlite_error(error))
                };
            }
        }
        transaction.commit().map_err(map_sqlite_error)?;
        self.get_project(project_id)
    }

    /// 绑定前置校验：标识合法且未被其他活动项目占用（归档占用者允许抢占）。
    /// 供「先创建再绑定」的调用方在创建前拦截，避免留下无绑定标识的项目。
    pub fn ensure_bind_target(&self, workspace_identifier: &str) -> Result<(), BusinessError> {
        let workspace = create_workspace_value(workspace_identifier)?;
        if let Some(occupant) = self.find_by_workspace_key(&workspace.key)?
            && !occupant.is_archived
        {
            return Err(BusinessError::with_message(
                ErrorCode::WorkspaceAlreadyBound,
                format!("该工作空间已绑定项目「{}」，请先解绑或更换标识", occupant.name),
            ));
        }
        Ok(())
    }

    /// 解除项目当前绑定的工作空间。
    pub fn unbind(&self, project_id: &str) -> Result<ProjectItem, BusinessError> {
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        let changed = connection
            .execute(
                "UPDATE project SET workspace_key=NULL, workspace_uri=NULL, workspace_label=NULL, \
                 workspace_last_seen_at=NULL, updated_at=$updated_at WHERE id=$id;",
                params![now_text, project_id],
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            return Err(BusinessError::new(ErrorCode::ProjectNotFound));
        }
        self.get_project(project_id)
    }

    /// 按工作空间保存项目记忆，首次调用缺少中文项目名时只返回询问信息。
    pub fn store_memory(&self, request: &WorkspaceMemoryRequest) -> Result<WorkspaceMemoryResult, BusinessError> {
        let workspace = create_workspace_value(&request.workspace_identifier)?;
        let project = self.find_by_workspace_key(&workspace.key)?;
        let project = match project {
            Some(project) => project,
            None if request.project_name.as_deref().map(str::trim).unwrap_or("").is_empty() => {
                return Ok(WorkspaceMemoryResult {
                    status: "PROJECT_NAME_REQUIRED".to_string(),
                    requires_user_input: true,
                    workspace_identifier: workspace.identifier,
                    question: Some(FIRST_BINDING_QUESTION.to_string()),
                    project: None,
                    memory: None,
                });
            }
            None => self.find_or_create_safely(&workspace, request.project_name.as_deref().unwrap_or(""))?,
        };
        ensure_project_active(&project)?;
        let save_request = memory_domain::SaveMemoryRequest {
            scope: MemoryScope::Project,
            project_id: Some(project.id.clone()),
            title: request.title.clone(),
            summary: request.summary.clone(),
            content: request.content.clone(),
            memory_type: request.memory_type.clone(),
            keywords: request.keywords.clone(),
            tags: request.tags.clone(),
            importance: request.importance,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: request.cloud_processing_allowed,
            expected_version: None,
        };
        // 与 C# 一致：经 CreateAsync 写入，来源固定为「桌面客户端」。
        let memory = self.memories.create(&save_request)?;
        self.touch(&project.id)?;
        Ok(WorkspaceMemoryResult {
            status: "STORED".to_string(),
            requires_user_input: false,
            workspace_identifier: workspace.identifier,
            question: None,
            project: Some(project),
            memory: Some(memory),
        })
    }

    /// 在进程内串行处理首次绑定，避免两个调用同时创建重复项目。
    fn find_or_create_safely(
        &self,
        workspace: &WorkspaceValue,
        project_name: &str,
    ) -> Result<ProjectItem, BusinessError> {
        let _guard = self
            .first_binding_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match self.find_by_workspace_key(&workspace.key)? {
            Some(existing) => Ok(existing),
            None => self.find_or_create_and_bind(workspace, project_name),
        }
    }

    /// 查找同名活动项目或创建新项目，然后完成工作空间绑定。
    fn find_or_create_and_bind(
        &self,
        workspace: &WorkspaceValue,
        project_name: &str,
    ) -> Result<ProjectItem, BusinessError> {
        let projects = self.projects.list(true)?;
        let trimmed = project_name.trim();
        let same_name = projects
            .iter()
            .find(|project| !project.is_archived && project.name.eq_ignore_ascii_case(trimmed));
        if let Some(project) = same_name
            && project.workspace_bound
        {
            return Err(BusinessError::with_message(
                ErrorCode::ProjectWorkspaceOccupied,
                "同名项目已经绑定其他工作空间，请更换名称或在客户端中重新绑定",
            ));
        }
        let project = match same_name {
            Some(project) => project.clone(),
            None => self.projects.create(&SaveProjectRequest {
                name: trimmed.to_string(),
                description: String::new(),
                color: DEFAULT_PROJECT_COLOR.to_string(),
            })?,
        };
        self.bind_workspace(&project.id, &workspace.identifier)
    }

    /// 按工作空间键查询项目。
    fn find_by_workspace_key(&self, workspace_key: &str) -> Result<Option<ProjectItem>, BusinessError> {
        let connection = self.database.open()?;
        let id: Option<String> = connection
            .query_row(
                "SELECT id FROM project WHERE workspace_key=$workspace_key;",
                params![workspace_key],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        match id {
            Some(id) => Ok(Some(self.get_project(&id)?)),
            None => Ok(None),
        }
    }

    /// 从项目服务读取指定项目（不存在时抛 PROJECT_NOT_FOUND）。
    fn get_project(&self, project_id: &str) -> Result<ProjectItem, BusinessError> {
        let connection = self.database.open()?;
        crate::project_service::get_project(&connection, project_id)
    }

    /// 更新工作空间最近使用时间。
    fn touch(&self, project_id: &str) -> Result<(), BusinessError> {
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        connection
            .execute(
                "UPDATE project SET workspace_last_seen_at=$seen_at WHERE id=$id;",
                params![now_text, project_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }
}

/// 经过标准化的工作空间值对象（C# `WorkspaceValue`）。
struct WorkspaceValue {
    identifier: String,
    key: String,
}

/// 创建经过标准化的工作空间值对象；标识无效抛 `WORKSPACE_IDENTIFIER_INVALID`。
fn create_workspace_value(workspace_identifier: &str) -> Result<WorkspaceValue, BusinessError> {
    let identifier = normalize_identifier(workspace_identifier)?;
    Ok(WorkspaceValue {
        identifier,
        key: calculate_key(&normalize_identifier(workspace_identifier)?),
    })
}

/// 确认项目存在且未归档（换绑允许：已绑定其他标识的项目可直接改绑新标识）。
fn ensure_project_can_bind(connection: &Connection, project_id: &str) -> Result<(), BusinessError> {
    let row: Option<i64> = connection
        .query_row(
            "SELECT is_archived FROM project WHERE id=$id;",
            params![project_id],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(map_sqlite_error)?;
    match row {
        None => Err(BusinessError::new(ErrorCode::ProjectNotFound)),
        Some(archived) if archived != 0 => Err(BusinessError::with_message(
            ErrorCode::ProjectArchived,
            "已归档项目不能绑定工作空间",
        )),
        Some(_) => Ok(()),
    }
}

/// 拒绝向已归档项目写入工作空间记忆。
fn ensure_project_active(project: &ProjectItem) -> Result<(), BusinessError> {
    if project.is_archived {
        return Err(BusinessError::new(ErrorCode::WorkspaceProjectArchived));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use chrono::{TimeZone, Utc};

    struct TestContext {
        workspace: WorkspaceService,
        projects: Arc<ProjectService>,
        _temp: tempfile::TempDir,
    }

    fn context() -> TestContext {
        let temp = tempfile::tempdir().unwrap();
        drop(memory_storage::open_initialized(&temp.path().join("test.db")).unwrap());
        let database = Database::new(temp.path().join("test.db"));
        let clock = Arc::new(FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap()));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=12)
                .map(|index| format!("{index:08}-{index:04}-4{index:03}-8{index:03}-{index:012}"))
                .collect(),
        ));
        let projects = Arc::new(ProjectService::new(database.clone(), clock.clone(), ids.clone()));
        let memories = Arc::new(MemoryService::new(database.clone(), clock.clone(), ids));
        let workspace = WorkspaceService::new(database, projects.clone(), memories, clock);
        TestContext {
            workspace,
            projects,
            _temp: temp,
        }
    }

    fn memory_request(workspace_identifier: &str, project_name: Option<&str>, content: &str) -> WorkspaceMemoryRequest {
        WorkspaceMemoryRequest {
            workspace_identifier: workspace_identifier.to_string(),
            project_name: project_name.map(str::to_string),
            title: "工作空间记忆".to_string(),
            summary: "摘要".to_string(),
            content: content.to_string(),
            memory_type: "NOTE".to_string(),
            keywords: vec![],
            tags: vec![],
            importance: 3,
            cloud_processing_allowed: false,
        }
    }

    #[test]
    fn resolve_unbound_asks_for_project_name() {
        let context = context();
        let resolution = context.workspace.resolve(r"E:\work\my-project").unwrap();
        assert_eq!(resolution.status, "PROJECT_NAME_REQUIRED");
        assert_eq!(resolution.workspace_identifier, "my-project");
        assert!(resolution.requires_user_input);
        assert!(resolution.question.is_some());
        // 大小写与分隔符归一后同一工作空间。
        let resolution2 = context.workspace.resolve("e:\\WORK\\MY-PROJECT").unwrap();
        assert_eq!(resolution2.workspace_identifier, "MY-PROJECT");
    }

    #[test]
    fn store_memory_without_name_asks_then_creates_and_maps() {
        let context = context();
        // 首次无项目名：仅询问。
        let result = context
            .workspace
            .store_memory(&memory_request(r"E:\work\alpha", None, "A 内容"))
            .unwrap();
        assert_eq!(result.status, "PROJECT_NAME_REQUIRED");
        assert!(result.requires_user_input);
        // 带项目名：创建项目 + 绑定 + 写入记忆。
        let result = context
            .workspace
            .store_memory(&memory_request(r"E:\work\alpha", Some("阿尔法项目"), "A 内容"))
            .unwrap();
        assert_eq!(result.status, "STORED");
        let project = result.project.as_ref().unwrap();
        assert_eq!(project.name, "阿尔法项目");
        assert!(project.workspace_bound);
        let memory = result.memory.as_ref().unwrap();
        assert_eq!(memory.scope, MemoryScope::Project);
        assert_eq!(memory.project_id, Some(project.id.clone()));
        // 再次写入：走已绑定路径（不再需要项目名）。
        let result = context
            .workspace
            .store_memory(&memory_request(r"E:\work\alpha", None, "B 内容"))
            .unwrap();
        assert_eq!(result.status, "STORED");
        // resolve 现在直接映射。
        let resolution = context.workspace.resolve(r"E:\work\alpha").unwrap();
        assert_eq!(resolution.status, "MAPPED");
        assert_eq!(resolution.project.as_ref().unwrap().name, "阿尔法项目");
    }

    #[test]
    fn store_memory_reuses_same_name_unbound_project() {
        let context = context();
        // 预建同名未绑定项目。
        context
            .projects
            .create(&SaveProjectRequest {
                name: "复用项目".to_string(),
                description: String::new(),
                color: "#238f7a".to_string(),
            })
            .unwrap();
        let result = context
            .workspace
            .store_memory(&memory_request("file:///E:/ws/reuse", Some("复用项目"), "复用内容"))
            .unwrap();
        assert_eq!(result.status, "STORED");
        assert!(result.project.as_ref().unwrap().workspace_bound);
        // 项目总数仍为 1。
        assert_eq!(context.projects.list(true).unwrap().len(), 1);
    }

    #[test]
    fn store_memory_rejects_archived_project() {
        let context = context();
        let result = context
            .workspace
            .store_memory(&memory_request(r"E:\ws\beta", Some("贝塔"), "B 内容"))
            .unwrap();
        let project_id = result.project.as_ref().unwrap().id.clone();
        context.projects.archive(&project_id).unwrap();
        let error = context
            .workspace
            .store_memory(&memory_request(r"E:\ws\beta", None, "C 内容"))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::WorkspaceProjectArchived);
    }

    #[test]
    fn unbind_allows_rebinding_to_other_workspace() {
        let context = context();
        context
            .workspace
            .store_memory(&memory_request(r"E:\ws\gamma", Some("伽马"), "G 内容"))
            .unwrap();
        let project_id = context.projects.list(true).unwrap()[0].id.clone();
        let unbound = context.workspace.unbind(&project_id).unwrap();
        assert!(!unbound.workspace_bound);
        // 换个工作空间可重新绑定。
        let rebound = context.workspace.bind_workspace(&project_id, r"E:\ws\delta").unwrap();
        assert!(rebound.workspace_bound);
        assert_eq!(rebound.workspace_identifier.as_deref(), Some("delta"));
    }

    #[test]
    fn bind_conflicts_are_reported() {
        let context = context();
        let project_a = context
            .projects
            .create(&SaveProjectRequest {
                name: "甲".to_string(),
                description: String::new(),
                color: "#111111".to_string(),
            })
            .unwrap();
        let project_b = context
            .projects
            .create(&SaveProjectRequest {
                name: "乙".to_string(),
                description: String::new(),
                color: "#222222".to_string(),
            })
            .unwrap();
        context.workspace.bind_workspace(&project_a.id, r"E:\ws\one").unwrap();
        // 编辑换绑：项目改绑新标识（未被占用）应成功。
        let rebound = context.workspace.bind_workspace(&project_a.id, r"E:\ws\two").unwrap();
        assert_eq!(rebound.workspace_identifier.as_deref(), Some("two"));
        // 另一项目绑到同一工作空间 → 报已占用（消息含占用项目名）。
        context.workspace.bind_workspace(&project_a.id, r"E:\ws\one").unwrap();
        let error = context
            .workspace
            .bind_workspace(&project_b.id, r"E:\ws\one")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::WorkspaceAlreadyBound);
        assert!(error.message.contains("甲"));
        // 归档项目不能绑定。
        context.projects.archive(&project_b.id).unwrap();
        let error = context
            .workspace
            .bind_workspace(&project_b.id, r"E:\ws\three")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectArchived);
        // 不存在的项目。
        let error = context
            .workspace
            .bind_workspace("00000000-0000-4000-8000-000000000099", r"E:\ws\four")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNotFound);
    }

    #[test]
    fn bind_takes_over_archived_occupant() {
        let context = context();
        // 项目甲绑定 one 后归档：归档项目继续占用标识。
        let project_a = context
            .projects
            .create(&SaveProjectRequest {
                name: "甲".to_string(),
                description: String::new(),
                color: "#111111".to_string(),
            })
            .unwrap();
        context.workspace.bind_workspace(&project_a.id, r"E:\ws\one").unwrap();
        context.projects.archive(&project_a.id).unwrap();
        // 新项目乙复用该标识：抢占成功，归档占用者绑定被清除。
        let project_b = context
            .projects
            .create(&SaveProjectRequest {
                name: "乙".to_string(),
                description: String::new(),
                color: "#222222".to_string(),
            })
            .unwrap();
        let bound = context.workspace.bind_workspace(&project_b.id, r"E:\ws\one").unwrap();
        assert_eq!(bound.workspace_identifier.as_deref(), Some("one"));
        let archived = context
            .projects
            .list(true)
            .unwrap()
            .into_iter()
            .find(|project| project.id == project_a.id)
            .unwrap();
        assert!(!archived.workspace_bound);
        assert!(archived.workspace_identifier.is_none());
    }

    #[test]
    fn invalid_identifier_rejected() {
        let context = context();
        let error = context
            .workspace
            .store_memory(&memory_request("   ", Some("任意"), "内容"))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::WorkspaceIdentifierInvalid);
    }
}
