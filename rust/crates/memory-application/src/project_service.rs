//! 项目服务：移植 C# `ProjectService`（创建、修改、归档、恢复与列表）。
//!
//! 语义对齐要点：
//! - 校验：名称 1-80 字符、颜色 1-20 字符（trim 后）。
//! - 唯一约束冲突（SQLite 主码 19）→ `PROJECT_NAME_EXISTS`。
//! - UPDATE 影响 0 行 → `PROJECT_NOT_FOUND`。
//! - create 返回值按 C# 直接构造（workspace 未绑定、计数 0），不回读数据库。

use std::sync::Arc;

use memory_domain::{BusinessError, DeletedProjectStats, ErrorCode, ProjectItem, SaveProjectRequest};
use rusqlite::{Connection, params};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::sqlite_errors::map_sqlite_error;

/// 项目行读取 SQL（与 C# `ProjectService` 一致，含工作空间列、活动记忆计数与全部记忆计数）。
pub(crate) const SELECT_PROJECT_SQL: &str = "
    SELECT p.id, p.name, p.description, p.color, p.is_archived,
           p.workspace_key, p.workspace_label,
           (SELECT count(*) FROM memory m WHERE m.project_id=p.id AND m.status='Active'),
           (SELECT count(*) FROM memory m WHERE m.project_id=p.id),
           p.created_at, p.updated_at
    FROM project p";

/// 个人项目的创建、修改和归档能力。
pub struct ProjectService {
    database: Database,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl ProjectService {
    /// 创建项目应用服务。
    pub fn new(database: Database, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self { database, clock, ids }
    }

    /// 查询项目列表（含/不含归档）。
    pub fn list(&self, include_archived: bool) -> Result<Vec<ProjectItem>, BusinessError> {
        let connection = self.database.open()?;
        let sql = format!(
            "{SELECT_PROJECT_SQL} \
             WHERE $include_archived = 1 OR p.is_archived = 0 \
             ORDER BY p.is_archived ASC, p.updated_at DESC, p.id DESC;"
        );
        let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
        let rows = statement
            .query_map(params![include_archived], read_project)
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(rows)
    }

    /// 创建一个活动项目。
    pub fn create(&self, request: &SaveProjectRequest) -> Result<ProjectItem, BusinessError> {
        validate_project(request)?;
        let id = self.ids.new_id();
        let now = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        let result = connection.execute(
            "INSERT INTO project(id, name, description, color, is_archived, created_at, updated_at) \
             VALUES($id, $name, $description, $color, 0, $created_at, $updated_at);",
            params![
                id,
                request.name.trim(),
                request.description.trim(),
                request.color.trim(),
                now,
                now,
            ],
        );
        match result {
            Ok(_) => {}
            Err(error) => {
                return if crate::sqlite_errors::unique_constraint_error(&error).is_some() {
                    Err(BusinessError::new(ErrorCode::ProjectNameExists))
                } else {
                    Err(map_sqlite_error(error))
                };
            }
        }
        // 与 C# 一致：返回直接构造的结果（不回读）。
        Ok(ProjectItem {
            id,
            name: request.name.trim().to_string(),
            description: request.description.trim().to_string(),
            color: request.color.trim().to_string(),
            is_archived: false,
            workspace_bound: false,
            workspace_identifier: None,
            active_memory_count: 0,
            total_memory_count: 0,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// 修改指定项目。
    pub fn update(&self, id: &str, request: &SaveProjectRequest) -> Result<ProjectItem, BusinessError> {
        validate_project(request)?;
        let now = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        match connection.execute(
            "UPDATE project SET name = $name, description = $description, color = $color, updated_at = $updated_at \
             WHERE id = $id;",
            params![
                request.name.trim(),
                request.description.trim(),
                request.color.trim(),
                now,
                id
            ],
        ) {
            Ok(0) => return Err(BusinessError::new(ErrorCode::ProjectNotFound)),
            Ok(_) => {}
            Err(error) => {
                return if crate::sqlite_errors::unique_constraint_error(&error).is_some() {
                    Err(BusinessError::new(ErrorCode::ProjectNameExists))
                } else {
                    Err(map_sqlite_error(error))
                };
            }
        }
        get_project(&connection, id)
    }

    /// 归档指定项目：事务内同步归档其全部活跃记忆（archived_at 记录本次时间）。
    ///
    /// 项目归档后不再接受新记忆（`validate_project` 拦截），联动归档保证
    /// 「全部记忆」不再出现已归档项目的记忆；恢复项目不反向恢复记忆。
    pub fn archive(&self, id: &str) -> Result<ProjectItem, BusinessError> {
        let now = format_storage_time(self.clock.now_utc());
        let mut connection = self.database.open()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        set_project_archived(&transaction, id, true, &now)?;
        transaction
            .execute(
                "UPDATE memory SET status='Archived', archived_at=$archived_at, \
                 updated_source=$source, updated_at=$updated_at \
                 WHERE project_id=$project_id AND status='Active';",
                params![now, "桌面客户端", now, id],
            )
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)?;
        let connection = self.database.open()?;
        get_project(&connection, id)
    }

    /// 恢复指定项目（已归档记忆保持归档，由用户在已归档视图中手动恢复）。
    pub fn restore(&self, id: &str) -> Result<ProjectItem, BusinessError> {
        self.set_archived(id, false)
    }

    /// 彻底删除已归档项目：物理删除项目及其全部记忆（含已归档）、候选、
    /// 搜索索引与后台任务；绑定该项目的 MCP 令牌解绑（project_id 置空）而非删除。
    /// 返回删除的记忆条数（含候选），供前端确认框与结果提示使用。
    pub fn delete_permanent(&self, id: &str) -> Result<DeletedProjectStats, BusinessError> {
        let connection = self.database.open()?;
        // 仅允许彻底删除已归档项目，活跃项目必须先归档。
        let archived: Option<i64> = connection
            .query_row("SELECT is_archived FROM project WHERE id=$id;", params![id], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        match archived {
            None => return Err(BusinessError::new(ErrorCode::ProjectNotFound)),
            Some(0) => return Err(BusinessError::new(ErrorCode::ProjectNotArchived)),
            Some(_) => {}
        }
        let mut connection = self.database.open()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let memory_ids = "SELECT id FROM memory WHERE project_id=$project_id";
        transaction
            .execute(
                &format!("DELETE FROM background_task WHERE target_id IN ({memory_ids});"),
                params![id],
            )
            .map_err(map_sqlite_error)?;
        transaction
            .execute(
                &format!("DELETE FROM memory_fts WHERE memory_id IN ({memory_ids});"),
                params![id],
            )
            .map_err(map_sqlite_error)?;
        let deleted_candidates = transaction
            .execute(
                "DELETE FROM memory_candidate WHERE project_id=$project_id;",
                params![id],
            )
            .map_err(map_sqlite_error)?;
        // ON DELETE CASCADE 带走修订 / 向量 / 关联行。
        let deleted_memories = transaction
            .execute("DELETE FROM memory WHERE project_id=$project_id;", params![id])
            .map_err(map_sqlite_error)?;
        transaction
            .execute(
                "UPDATE mcp_token SET project_id=NULL WHERE project_id=$project_id;",
                params![id],
            )
            .map_err(map_sqlite_error)?;
        let deleted_projects = transaction
            .execute("DELETE FROM project WHERE id=$id;", params![id])
            .map_err(map_sqlite_error)?;
        debug_assert_eq!(deleted_projects, 1);
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(DeletedProjectStats {
            deleted_memories: deleted_memories as i64,
            deleted_candidates: deleted_candidates as i64,
        })
    }

    fn set_archived(&self, id: &str, archived: bool) -> Result<ProjectItem, BusinessError> {
        let now = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        set_project_archived(&connection, id, archived, &now)?;
        get_project(&connection, id)
    }
}

/// 更新项目归档状态（供工作空间服务等复用）。
pub(crate) fn set_project_archived(
    connection: &Connection,
    id: &str,
    archived: bool,
    now: &str,
) -> Result<(), BusinessError> {
    let changed = connection
        .execute(
            "UPDATE project SET is_archived = $archived, updated_at = $updated_at WHERE id = $id;",
            params![archived, now, id],
        )
        .map_err(map_sqlite_error)?;
    if changed == 0 {
        return Err(BusinessError::new(ErrorCode::ProjectNotFound));
    }
    Ok(())
}

/// 读取指定项目（回读，workspace 列与记忆计数取实时值）。
pub(crate) fn get_project(connection: &Connection, id: &str) -> Result<ProjectItem, BusinessError> {
    let sql = format!("{SELECT_PROJECT_SQL} WHERE p.id = $id;");
    connection
        .query_row(&sql, params![id], read_project)
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => BusinessError::new(ErrorCode::ProjectNotFound),
            other => map_sqlite_error(other),
        })
}

/// 字符长度按 UTF-16 code unit 计数（与 C# `string.Length` 一致）。
fn utf16_length(value: &str) -> usize {
    value.chars().map(|c| c.len_utf16()).sum()
}

/// 校验项目输入（与 C# `Validate` 一致）。
fn validate_project(request: &SaveProjectRequest) -> Result<(), BusinessError> {
    if request.name.trim().is_empty() || utf16_length(request.name.trim()) > 80 {
        return Err(BusinessError::new(ErrorCode::ProjectNameInvalid));
    }
    if request.color.trim().is_empty() || utf16_length(request.color.trim()) > 20 {
        return Err(BusinessError::new(ErrorCode::ProjectColorInvalid));
    }
    Ok(())
}

/// 从行构造项目（与 C# `ReadProject` 一致：bound = workspace_key 非空，identifier = workspace_label）。
pub(crate) fn read_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectItem> {
    Ok(ProjectItem {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        color: row.get(3)?,
        is_archived: row.get::<_, i64>(4)? != 0,
        workspace_bound: !row.get::<_, Option<String>>(5)?.is_none(),
        workspace_identifier: row.get(6)?,
        active_memory_count: row.get(7)?,
        total_memory_count: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use chrono::{TimeZone, Utc};

    fn service(temp: &tempfile::TempDir) -> ProjectService {
        drop(memory_storage::open_initialized(&temp.path().join("test.db")).unwrap());
        let database = Database::new(temp.path().join("test.db"));
        let clock = FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap());
        let ids = FixedIdGenerator::new(vec!["11111111-1111-4111-8111-111111111111".to_string()]);
        ProjectService::new(database, Arc::new(clock), Arc::new(ids))
    }

    fn request(name: &str) -> SaveProjectRequest {
        SaveProjectRequest {
            name: name.to_string(),
            description: "描述".to_string(),
            color: "#238f7a".to_string(),
        }
    }

    #[test]
    fn create_then_list_update_archive_restore() {
        let temp = tempfile::tempdir().unwrap();
        let service = service(&temp);
        let created = service.create(&request("我的项目")).unwrap();
        assert_eq!(created.name, "我的项目");
        assert!(!created.is_archived);
        assert_eq!(created.created_at, "2026-08-15T08:00:00.0000000+00:00");

        let list = service.list(false).unwrap();
        assert_eq!(list.len(), 1);

        let updated = service.update(&created.id, &request("新名称")).unwrap();
        assert_eq!(updated.name, "新名称");

        let archived = service.archive(&created.id).unwrap();
        assert!(archived.is_archived);
        // 未含归档的列表不出现，含归档的出现。
        assert!(service.list(false).unwrap().is_empty());
        assert_eq!(service.list(true).unwrap().len(), 1);

        let restored = service.restore(&created.id).unwrap();
        assert!(!restored.is_archived);
    }

    #[test]
    fn duplicate_name_raises_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let service = service(&temp);
        service.create(&request("同名")).unwrap();
        let error = service.create(&request("同名")).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNameExists);
        // 大小写不敏感唯一（COLLATE NOCASE）。
        let error = service.create(&request("同名")).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNameExists);
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let service = service(&temp);
        let error = service.create(&request("  ")).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNameInvalid);
        let mut bad_color = request("正常名");
        bad_color.color = " ".to_string();
        let error = service.create(&bad_color).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectColorInvalid);
    }

    #[test]
    fn missing_project_raises_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let service = service(&temp);
        let error = service
            .update("00000000-0000-4000-8000-000000000001", &request("x"))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNotFound);
        let error = service.archive("00000000-0000-4000-8000-000000000001").unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNotFound);
        let error = service.restore("00000000-0000-4000-8000-000000000001").unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNotFound);
        let error = service
            .delete_permanent("00000000-0000-4000-8000-000000000001")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNotFound);
    }

    /// 构造带 MemoryService 的上下文：归档联动 / 彻底删除需要真实记忆行。
    fn context(temp: &tempfile::TempDir) -> (ProjectService, crate::memory_service::MemoryService) {
        drop(memory_storage::open_initialized(&temp.path().join("test.db")).unwrap());
        let database = Database::new(temp.path().join("test.db"));
        let clock = Arc::new(FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap()));
        let ids = Arc::new(FixedIdGenerator::new(vec![
            "22222222-2222-4222-8222-222222222222".to_string(),
        ]));
        (
            ProjectService::new(database.clone(), clock.clone(), ids.clone()),
            crate::memory_service::MemoryService::new(database, clock, ids),
        )
    }

    fn memory_request(project_id: &str) -> memory_domain::SaveMemoryRequest {
        memory_domain::SaveMemoryRequest {
            scope: memory_domain::MemoryScope::Project,
            project_id: Some(project_id.to_string()),
            title: "项目记忆".to_string(),
            summary: "摘要".to_string(),
            content: "项目归档联动测试用的记忆正文".to_string(),
            memory_type: "NOTE".to_string(),
            keywords: vec![],
            tags: vec![],
            importance: 3,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: false,
            expected_version: None,
        }
    }

    #[test]
    fn archive_archives_active_memories_together() {
        let temp = tempfile::tempdir().unwrap();
        let (projects, memories) = context(&temp);
        let project = projects.create(&request("联动项目")).unwrap();
        memories.create(&memory_request(&project.id)).unwrap();
        let archived = projects.archive(&project.id).unwrap();
        assert_eq!(archived.active_memory_count, 0);
        assert_eq!(archived.total_memory_count, 1);
        let connection = projects.database.open().unwrap();
        let status: String = connection
            .query_row(
                "SELECT status FROM memory WHERE project_id=$id;",
                params![project.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "Archived");
    }

    #[test]
    fn restore_keeps_memories_archived() {
        let temp = tempfile::tempdir().unwrap();
        let (projects, memories) = context(&temp);
        let project = projects.create(&request("恢复不联动")).unwrap();
        memories.create(&memory_request(&project.id)).unwrap();
        projects.archive(&project.id).unwrap();
        let restored = projects.restore(&project.id).unwrap();
        assert!(!restored.is_archived);
        assert_eq!(restored.active_memory_count, 0);
        assert_eq!(restored.total_memory_count, 1);
    }

    #[test]
    fn delete_permanent_removes_project_with_all_data() {
        let temp = tempfile::tempdir().unwrap();
        let (projects, memories) = context(&temp);
        let project = projects.create(&request("待删除")).unwrap();
        memories.create(&memory_request(&project.id)).unwrap();
        projects.archive(&project.id).unwrap();
        let stats = projects.delete_permanent(&project.id).unwrap();
        assert_eq!(stats.deleted_memories, 1);
        assert!(projects.list(true).unwrap().is_empty());
        let connection = projects.database.open().unwrap();
        for table in ["memory", "memory_fts", "memory_candidate", "background_task"] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table};"), [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} 应被清空");
        }
    }

    #[test]
    fn delete_permanent_rejects_active_project() {
        let temp = tempfile::tempdir().unwrap();
        let (projects, _memories) = context(&temp);
        let project = projects.create(&request("活跃项目")).unwrap();
        let error = projects.delete_permanent(&project.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNotArchived);
    }
}
