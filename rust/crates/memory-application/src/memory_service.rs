//! 记忆服务：移植 C# `MemoryService`（写入、版本、归档、分页与候选确认事务）。
//!
//! 语义对齐要点（全部保持 C# 行为）：
//! - 四处事务：create_from_source / confirm_candidate / update_from_source / delete_permanently。
//! - 乐观锁双保险：应用层比对 ExpectedVersion + `UPDATE ... WHERE version=$expected` 兜底。
//! - 历史版本仅保留上一版快照：每次 versioned 变更先 DELETE 全部旧 revision 再插入当前版。
//! - 收藏/置顶不触发版本递增（`has_versioned_changes` 白名单）。
//! - 唯一约束冲突（SQLite 主码 19）→ `MEMORY_DUPLICATE`。

use std::sync::Arc;

use memory_domain::{
    BusinessError, CursorPage, ErrorCode, MemoryFacets, MemoryItem, MemoryListQuery, MemoryRevisionItem, MemoryScope,
    MemoryStatus, QuickCaptureRequest, SaveMemoryRequest, content_checksum, decode_cursor, encode_cursor,
    is_valid_memory_type,
};
use rusqlite::{Connection, Transaction, named_params, params, types::Value as SqlValue};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::sqlite_errors::{map_sqlite_error, unique_constraint_error};
use crate::tokenizer::tokenize;

/// 桌面端默认来源名（与 C# 常量一致）。
pub const DESKTOP_SOURCE: &str = "桌面客户端";

/// 记忆行读取 SQL（与 C# `SelectMemorySql` 一致，project_name 来自 LEFT JOIN）。
pub(crate) const SELECT_MEMORY_SQL: &str = "
    SELECT m.id,m.scope,m.project_id,p.name,m.title,m.summary,m.content,m.memory_type, \
           m.keywords_json,m.tags_json,m.importance,m.is_favorite,m.is_pinned,m.cloud_processing_allowed, \
           m.status,m.version,m.created_source,m.updated_source,m.created_at,m.updated_at,m.archived_at \
    FROM memory m LEFT JOIN project p ON p.id=m.project_id";

/// 记忆写事务内的 SQLite 错误映射：唯一约束冲突（主码 19）→ `MEMORY_DUPLICATE`
/// （与 C# `catch (SqliteException e) when e.SqliteErrorCode == 19` 一致）。
fn map_memory_write_error(error: rusqlite::Error) -> BusinessError {
    if unique_constraint_error(&error).is_some() {
        return BusinessError::new(ErrorCode::MemoryDuplicate);
    }
    map_sqlite_error(error)
}

/// 提供记忆写入、版本、归档和分页读取能力。
pub struct MemoryService {
    database: Database,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl MemoryService {
    /// 创建记忆应用服务。
    pub fn new(database: Database, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self { database, clock, ids }
    }

    /// 创建一条记忆并同步索引（来源 = 桌面客户端）。
    pub fn create(&self, request: &SaveMemoryRequest) -> Result<MemoryItem, BusinessError> {
        self.create_from_source(request, DESKTOP_SOURCE)
    }

    /// 使用可信调用来源创建记忆并同步索引。
    pub fn create_from_source(
        &self,
        request: &SaveMemoryRequest,
        source_name: &str,
    ) -> Result<MemoryItem, BusinessError> {
        validate_memory(request, false)?;
        let id = self.ids.new_id();
        let now_text = format_storage_time(self.clock.now_utc());
        let checksum = content_checksum(&request.content);
        let mut connection = self.database.open()?;
        validate_project(&connection, request.scope, request.project_id.as_deref())?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        insert_memory(&transaction, &id, request, &checksum, source_name, &now_text)
            .and_then(|_| upsert_fts(&transaction, &id, request))
            .and_then(|_| self.queue_graph_recompute(&transaction, &id, &now_text))
            .and_then(|_| {
                if request.cloud_processing_allowed {
                    self.queue_embedding(&transaction, &id, &now_text)
                } else {
                    Ok(())
                }
            })?;
        transaction.commit().map_err(map_sqlite_error)?;
        get_memory(&connection, &id)
    }

    /// 在同一事务中把候选写入正式记忆、索引和任务，并删除原候选。
    pub fn confirm_candidate(
        &self,
        candidate_id: &str,
        expected_candidate_version: i64,
        request: &SaveMemoryRequest,
        source_name: &str,
    ) -> Result<MemoryItem, BusinessError> {
        validate_memory(request, false)?;
        let memory_id = self.ids.new_id();
        let now_text = format_storage_time(self.clock.now_utc());
        let checksum = content_checksum(&request.content);
        let mut connection = self.database.open()?;
        validate_project(&connection, request.scope, request.project_id.as_deref())?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        insert_memory(&transaction, &memory_id, request, &checksum, source_name, &now_text)
            .and_then(|_| upsert_fts(&transaction, &memory_id, request))
            .and_then(|_| self.queue_graph_recompute(&transaction, &memory_id, &now_text))
            .and_then(|_| {
                if request.cloud_processing_allowed {
                    self.queue_embedding(&transaction, &memory_id, &now_text)
                } else {
                    Ok(())
                }
            })
            .and_then(|_| {
                let deleted = transaction
                    .execute(
                        "DELETE FROM memory_candidate WHERE id=$id AND status='PENDING' AND version=$version;",
                        params![candidate_id, expected_candidate_version],
                    )
                    .map_err(map_sqlite_error)?;
                if deleted == 0 {
                    return Err(BusinessError::with_message(
                        ErrorCode::MemoryCandidateVersionConflict,
                        "候选确认状态发生冲突",
                    ));
                }
                Ok(())
            })?;
        transaction.commit().map_err(map_sqlite_error)?;
        get_memory(&connection, &memory_id)
    }

    /// 使用正文生成标题并快速保存个人记忆。
    pub fn quick_capture(&self, request: &QuickCaptureRequest) -> Result<MemoryItem, BusinessError> {
        let content = request.content.trim().to_string();
        if content.is_empty() {
            return Err(BusinessError::new(ErrorCode::MemoryContentRequired));
        }
        let first_line = content.lines().next().unwrap_or("").trim();
        let title: String = if first_line.chars().map(|c| c.len_utf16()).sum::<usize>() <= 60 {
            first_line.to_string()
        } else {
            let mut truncated = String::new();
            let mut length = 0;
            for character in first_line.chars() {
                if length + character.len_utf16() > 60 {
                    break;
                }
                truncated.push(character);
                length += character.len_utf16();
            }
            truncated
        };
        let save_request = SaveMemoryRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title,
            summary: String::new(),
            content,
            memory_type: "NOTE".to_string(),
            keywords: vec![],
            tags: vec![],
            importance: 3,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: true,
            expected_version: None,
        };
        self.create(&save_request)
    }

    /// 修改记忆（来源 = 桌面客户端）。
    pub fn update(&self, id: &str, request: &SaveMemoryRequest) -> Result<MemoryItem, BusinessError> {
        self.update_from_source(id, request, DESKTOP_SOURCE)
    }

    /// 使用可信调用来源修改记忆并保留乐观锁语义。
    pub fn update_from_source(
        &self,
        id: &str,
        request: &SaveMemoryRequest,
        source_name: &str,
    ) -> Result<MemoryItem, BusinessError> {
        validate_memory(request, true)?;
        let mut connection = self.database.open()?;
        validate_project(&connection, request.scope, request.project_id.as_deref())?;
        let current = get_memory(&connection, id)?;
        if current.status == MemoryStatus::Archived {
            return Err(BusinessError::new(ErrorCode::MemoryArchivedReadOnly));
        }
        if request.expected_version != Some(current.version) {
            return Err(BusinessError::new(ErrorCode::MemoryVersionConflict));
        }
        let versioned_changes = has_versioned_changes(&current, request);
        let next_version = if versioned_changes {
            current.version + 1
        } else {
            current.version
        };
        let now_text = format_storage_time(self.clock.now_utc());
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let updated = transaction
            .execute(
                "UPDATE memory SET scope=$scope, project_id=$project_id, title=$title, summary=$summary, \
                 content=$content, memory_type=$memory_type, keywords_json=$keywords_json, tags_json=$tags_json, \
                 importance=$importance, is_favorite=$is_favorite, is_pinned=$is_pinned, \
                 cloud_processing_allowed=$cloud_processing_allowed, version=$next_version, \
                 content_checksum=$checksum, updated_source=$updated_source, updated_at=$updated_at \
                 WHERE id=$id AND version=$expected_version;",
                memory_update_params(
                    id,
                    request,
                    &content_checksum(&request.content),
                    &now_text,
                    next_version,
                    current.version,
                    &normalize_source(source_name)?,
                )
                .as_slice(),
            )
            .map_err(map_memory_write_error)?;
        if updated == 0 {
            return Err(BusinessError::new(ErrorCode::MemoryVersionConflict));
        }
        if versioned_changes {
            self.replace_previous_revision(&transaction, &current, &now_text)
                .and_then(|_| upsert_fts(&transaction, id, request))
                .and_then(|_| self.queue_graph_recompute(&transaction, id, &now_text))
                .and_then(|_| {
                    if request.cloud_processing_allowed {
                        self.queue_embedding(&transaction, id, &now_text)
                    } else {
                        delete_embedding(&transaction, id)
                    }
                })?;
        }
        transaction.commit().map_err(map_sqlite_error)?;
        get_memory(&connection, id)
    }

    /// 读取一条完整记忆。
    pub fn get(&self, id: &str) -> Result<MemoryItem, BusinessError> {
        let connection = self.database.open()?;
        get_memory(&connection, id)
    }

    /// 按条件读取游标分页记忆。
    pub fn list(&self, query: &MemoryListQuery) -> Result<CursorPage<MemoryItem>, BusinessError> {
        let size = query.size.clamp(1, 100);
        let cursor = decode_cursor(query.cursor.as_deref())?;
        let connection = self.database.open()?;
        let status = query
            .status
            .unwrap_or(MemoryStatus::Active)
            .as_status_text()
            .to_string();
        let scope: Option<String> = query.scope.map(|value| value.as_scope_text().to_string());
        let memory_type = query
            .memory_type
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let tag = query
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let mut statement = connection
            .prepare(&format!(
                "{SELECT_MEMORY_SQL} \
                 WHERE m.status=$status \
                   AND ($scope IS NULL OR m.scope=$scope) \
                   AND ($project_id IS NULL OR m.project_id=$project_id) \
                   AND ($is_favorite IS NULL OR m.is_favorite=$is_favorite) \
                   AND ($is_pinned IS NULL OR m.is_pinned=$is_pinned) \
                   AND ($type IS NULL OR m.memory_type=$type) \
                   AND ($tag IS NULL OR m.tags_json LIKE '%' || $tag || '%') \
                   AND ($importance_min IS NULL OR m.importance >= $importance_min) \
                   AND ($cursor_pinned IS NULL \
                        OR m.is_pinned < $cursor_pinned \
                        OR (m.is_pinned = $cursor_pinned AND m.updated_at < $cursor_time) \
                        OR (m.is_pinned = $cursor_pinned AND m.updated_at = $cursor_time AND m.id < $cursor_id)) \
                 ORDER BY m.is_pinned DESC, m.updated_at DESC, m.id DESC \
                 LIMIT $limit;"
            ))
            .map_err(map_sqlite_error)?;
        let bindings: Vec<(&str, SqlValue)> = vec![
            ("$status", status.into()),
            ("$scope", scope.into()),
            ("$project_id", query.project_id.clone().into()),
            (
                "$is_favorite",
                query.is_favorite.map(|value| SqlValue::from(value as i64)).into(),
            ),
            (
                "$is_pinned",
                query.is_pinned.map(|value| SqlValue::from(value as i64)).into(),
            ),
            ("$type", memory_type.into()),
            ("$tag", tag.into()),
            ("$importance_min", query.importance_min.into()),
            (
                "$cursor_pinned",
                cursor
                    .as_ref()
                    .map(|value| SqlValue::from(value.is_pinned as i64))
                    .into(),
            ),
            (
                "$cursor_time",
                cursor
                    .as_ref()
                    .map(|value| SqlValue::Text(value.updated_at.clone()))
                    .into(),
            ),
            (
                "$cursor_id",
                cursor.as_ref().map(|value| SqlValue::Text(value.id.clone())).into(),
            ),
            ("$limit", SqlValue::from(size + 1)),
        ];
        let rows = statement
            .query_map(bindings.as_slice(), read_memory)
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        let mut memories = rows;
        let has_more = memories.len() > size as usize;
        if has_more {
            memories.pop();
        }
        let next_cursor = if has_more && !memories.is_empty() {
            let last = memories.last().unwrap();
            Some(encode_cursor(last.is_pinned, &last.updated_at, &last.id))
        } else {
            None
        };
        Ok(CursorPage {
            items: memories,
            next_cursor,
            has_more,
        })
    }

    /// 聚合记忆页全部分类与项目的数量。
    pub fn get_facets(&self) -> Result<MemoryFacets, BusinessError> {
        let connection = self.database.open()?;
        let (all, personal, project, favorite, pinned, archived): (i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT \
                        sum(CASE WHEN status='Active' THEN 1 ELSE 0 END), \
                        sum(CASE WHEN status='Active' AND scope='Personal' THEN 1 ELSE 0 END), \
                        sum(CASE WHEN status='Active' AND scope='Project' THEN 1 ELSE 0 END), \
                        sum(CASE WHEN status='Active' AND is_favorite=1 THEN 1 ELSE 0 END), \
                        sum(CASE WHEN status='Active' AND is_pinned=1 THEN 1 ELSE 0 END), \
                        sum(CASE WHEN status='Archived' THEN 1 ELSE 0 END) \
                     FROM memory;",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    ))
                },
            )
            .map_err(map_sqlite_error)?;
        let mut statement = connection
            .prepare(
                "SELECT project_id, count(*) FROM memory \
                 WHERE status='Active' AND project_id IS NOT NULL GROUP BY project_id;",
            )
            .map_err(map_sqlite_error)?;
        let project_counts = statement
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(map_sqlite_error)?
            .collect::<Result<std::collections::BTreeMap<String, i64>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(MemoryFacets {
            all_count: all,
            personal_count: personal,
            project_count: project,
            favorite_count: favorite,
            pinned_count: pinned,
            archived_count: archived,
            project_counts,
        })
    }

    /// 归档指定记忆（来源 = 桌面客户端）。
    pub fn archive(&self, id: &str) -> Result<MemoryItem, BusinessError> {
        self.set_status(id, MemoryStatus::Archived, DESKTOP_SOURCE)
    }

    /// 使用可信调用来源归档指定记忆。
    pub fn archive_from_source(&self, id: &str, source_name: &str) -> Result<MemoryItem, BusinessError> {
        self.set_status(id, MemoryStatus::Archived, source_name)
    }

    /// 恢复指定记忆（来源 = 桌面客户端）。
    ///
    /// 项目记忆的所属项目已归档时拒绝恢复（避免恢复出孤儿记忆），
    /// 提示用户先在「已归档」中恢复项目。
    pub fn restore(&self, id: &str) -> Result<MemoryItem, BusinessError> {
        let connection = self.database.open()?;
        // scope + 项目归档状态 + 项目名一次取出；记忆不存在时跳过校验
        // 交由 set_status 统一报 MEMORY_NOT_FOUND。
        let project_state: Option<(Option<i64>, Option<String>)> = connection
            .query_row(
                "SELECT (SELECT is_archived FROM project WHERE project.id = memory.project_id), \
                 (SELECT name FROM project WHERE project.id = memory.project_id) \
                 FROM memory WHERE id=$id AND scope='Project';",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        if let Some((Some(archived), project_name)) = project_state
            && archived != 0
        {
            return Err(BusinessError::with_message(
                ErrorCode::ProjectArchived,
                format!(
                    "所属项目「{}」已归档，请先在「已归档-已归档项目」中恢复项目，再恢复该记忆",
                    project_name.unwrap_or_default()
                ),
            ));
        }
        self.set_status(id, MemoryStatus::Active, DESKTOP_SOURCE)
    }

    /// 永久删除已归档记忆及其本地索引、任务和关联数据。
    pub fn delete_permanently(&self, id: &str) -> Result<(), BusinessError> {
        let mut connection = self.database.open()?;
        let current = get_memory(&connection, id)?;
        if current.status != MemoryStatus::Archived {
            return Err(BusinessError::new(ErrorCode::MemoryNotArchived));
        }
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        transaction
            .execute("DELETE FROM background_task WHERE target_id=$id;", params![id])
            .map_err(map_sqlite_error)?;
        transaction
            .execute("DELETE FROM memory_fts WHERE memory_id=$id;", params![id])
            .map_err(map_sqlite_error)?;
        let deleted = transaction
            .execute("DELETE FROM memory WHERE id=$id AND status='Archived';", params![id])
            .map_err(map_sqlite_error)?;
        if deleted == 0 {
            return Err(BusinessError::new(ErrorCode::MemoryNotArchived));
        }
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 查询记忆可恢复的上一版，最多返回一项。
    pub fn list_revisions(&self, id: &str) -> Result<Vec<MemoryRevisionItem>, BusinessError> {
        let connection = self.database.open()?;
        let mut statement = connection
            .prepare(
                "SELECT id,memory_id,version,created_at FROM memory_revision \
                 WHERE memory_id=$id ORDER BY version DESC LIMIT 1;",
            )
            .map_err(map_sqlite_error)?;
        let items = statement
            .query_map(params![id], |row| {
                Ok(MemoryRevisionItem {
                    id: row.get(0)?,
                    memory_id: row.get(1)?,
                    version: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(items)
    }

    /// 恢复上一版；恢复前的当前内容会成为新的上一版。
    pub fn restore_revision(&self, id: &str, version: i64) -> Result<MemoryItem, BusinessError> {
        let connection = self.database.open()?;
        let snapshot: Option<String> = connection
            .query_row(
                "SELECT snapshot_json FROM memory_revision WHERE memory_id=$id AND version=$version;",
                params![id, version],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        let Some(snapshot) = snapshot else {
            return Err(BusinessError::new(ErrorCode::MemoryRevisionNotFound));
        };
        let mut restored: SaveMemoryRequest =
            serde_json::from_str(&snapshot).map_err(|_| BusinessError::new(ErrorCode::MemoryRevisionInvalid))?;
        let current = get_memory(&connection, id)?;
        restored.expected_version = Some(current.version);
        self.update(id, &restored)
    }

    /// 更新记忆状态；归档时同事务删除图谱边，恢复时同事务排队关系重算。
    fn set_status(&self, id: &str, status: MemoryStatus, source_name: &str) -> Result<MemoryItem, BusinessError> {
        let now_text = format_storage_time(self.clock.now_utc());
        let mut connection = self.database.open()?;
        let archived_at: Option<String> = if status == MemoryStatus::Archived {
            Some(now_text.clone())
        } else {
            None
        };
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let changed = transaction
            .execute(
                "UPDATE memory SET status=$status, archived_at=$archived_at, \
                 updated_source=$source, updated_at=$updated_at WHERE id=$id;",
                named_params! {
                    "$status": status.as_status_text(),
                    "$archived_at": archived_at,
                    "$source": normalize_source(source_name)?,
                    "$updated_at": now_text,
                    "$id": id,
                },
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            return Err(BusinessError::new(ErrorCode::MemoryNotFound));
        }
        if status == MemoryStatus::Archived {
            // 归档记忆不进入图谱：立即删除相关边（永久删除由外键级联兜底）。
            transaction
                .execute(
                    "DELETE FROM memory_edge WHERE memory_id_a=$id OR memory_id_b=$id;",
                    params![id],
                )
                .map_err(map_sqlite_error)?;
        } else {
            self.queue_graph_recompute(&transaction, id, &now_text)?;
        }
        transaction.commit().map_err(map_sqlite_error)?;
        get_memory(&connection, id)
    }

    /// 用修改前的当前状态替换唯一的上一版快照。
    fn replace_previous_revision(
        &self,
        transaction: &Transaction<'_>,
        current: &MemoryItem,
        now_text: &str,
    ) -> Result<(), BusinessError> {
        transaction
            .execute(
                "DELETE FROM memory_revision WHERE memory_id=$memory_id;",
                params![current.id],
            )
            .map_err(map_memory_write_error)?;
        let snapshot = create_snapshot_request(current);
        let snapshot_json = serde_json::to_string(&snapshot).map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("快照序列化失败：{error}"))
        })?;
        transaction
            .execute(
                "INSERT INTO memory_revision(id,memory_id,version,snapshot_json,created_at) \
                 VALUES($id,$memory_id,$version,$snapshot,$created_at);",
                params![self.ids.new_id(), current.id, current.version, snapshot_json, now_text],
            )
            .map_err(map_memory_write_error)?;
        Ok(())
    }

    /// 创建记忆向量后台任务，并去除同记忆的重复活动任务。
    fn queue_embedding(
        &self,
        transaction: &Transaction<'_>,
        memory_id: &str,
        now_text: &str,
    ) -> Result<(), BusinessError> {
        transaction
            .execute(
                "DELETE FROM background_task WHERE task_type='EMBED_MEMORY' AND target_id=$target_id \
                 AND status IN ('PENDING','RUNNING');",
                params![memory_id],
            )
            .map_err(map_memory_write_error)?;
        transaction
            .execute(
                "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at, \
                 error_code,error_message,created_at,updated_at) \
                 VALUES($id,'EMBED_MEMORY',$target_id,'PENDING',0,$next,NULL,NULL,$created,$updated);",
                params![self.ids.new_id(), memory_id, now_text, now_text, now_text],
            )
            .map_err(map_memory_write_error)?;
        Ok(())
    }

    /// 排队单记忆关系重算（REBUILD_GRAPH_MEMORY），并去除同记忆的重复活动任务。
    fn queue_graph_recompute(
        &self,
        transaction: &Transaction<'_>,
        memory_id: &str,
        now_text: &str,
    ) -> Result<(), BusinessError> {
        transaction
            .execute(
                "DELETE FROM background_task WHERE task_type='REBUILD_GRAPH_MEMORY' AND target_id=$target_id \
                 AND status IN ('PENDING','RUNNING');",
                params![memory_id],
            )
            .map_err(map_memory_write_error)?;
        transaction
            .execute(
                "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at, \
                 error_code,error_message,created_at,updated_at) \
                 VALUES($id,'REBUILD_GRAPH_MEMORY',$target_id,'PENDING',0,$next,NULL,NULL,$created,$updated);",
                params![self.ids.new_id(), memory_id, now_text, now_text, now_text],
            )
            .map_err(map_memory_write_error)?;
        Ok(())
    }
}

/// 从已打开连接读取记忆（与 C# `GetAsync(connection, id)` 一致）。
pub(crate) fn get_memory(connection: &Connection, id: &str) -> Result<MemoryItem, BusinessError> {
    let sql = format!("{SELECT_MEMORY_SQL} WHERE m.id=$id;");
    connection
        .query_row(&sql, params![id], read_memory)
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => BusinessError::new(ErrorCode::MemoryNotFound),
            other => map_sqlite_error(other),
        })
}

/// 从行构造记忆（与 C# `ReadMemory` 一致；keywords/tags 解析 JSON 数组）。
pub(crate) fn read_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryItem> {
    let scope_text: String = row.get(1)?;
    let status_text: String = row.get(14)?;
    Ok(MemoryItem {
        id: row.get(0)?,
        scope: if scope_text == "Personal" {
            MemoryScope::Personal
        } else {
            MemoryScope::Project
        },
        project_id: row.get(2)?,
        project_name: row.get(3)?,
        title: row.get(4)?,
        summary: row.get(5)?,
        content: row.get(6)?,
        memory_type: row.get(7)?,
        keywords: parse_json_list(&row.get::<_, String>(8)?),
        tags: parse_json_list(&row.get::<_, String>(9)?),
        importance: row.get(10)?,
        is_favorite: row.get::<_, i64>(11)? != 0,
        is_pinned: row.get::<_, i64>(12)? != 0,
        cloud_processing_allowed: row.get::<_, i64>(13)? != 0,
        status: if status_text == "Active" {
            MemoryStatus::Active
        } else {
            MemoryStatus::Archived
        },
        version: row.get(15)?,
        created_source: row.get(16)?,
        updated_source: row.get(17)?,
        created_at: row.get(18)?,
        updated_at: row.get(19)?,
        archived_at: row.get(20)?,
    })
}

fn parse_json_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

/// 写入新记忆（与 C# `InsertMemoryAsync` 一致）。
fn insert_memory(
    transaction: &Transaction<'_>,
    id: &str,
    request: &SaveMemoryRequest,
    checksum: &str,
    source_name: &str,
    now_text: &str,
) -> Result<(), BusinessError> {
    transaction
        .execute(
            "INSERT INTO memory(id,scope,project_id,title,summary,content,memory_type,keywords_json,tags_json, \
             importance,is_favorite,is_pinned,cloud_processing_allowed,status,version,content_checksum, \
             created_source,updated_source,created_at,updated_at,archived_at) \
             VALUES($id,$scope,$project_id,$title,$summary,$content,$memory_type,$keywords_json,$tags_json, \
             $importance,$is_favorite,$is_pinned,$cloud_processing_allowed,'Active',1,$checksum, \
             $source,$source,$created_at,$updated_at,NULL);",
            memory_insert_params(id, request, checksum, source_name, now_text).as_slice(),
        )
        .map_err(map_memory_write_error)?;
    Ok(())
}

/// 创建或替换全文索引（token 列用共享分词器）。
fn upsert_fts(
    transaction: &Transaction<'_>,
    memory_id: &str,
    request: &SaveMemoryRequest,
) -> Result<(), BusinessError> {
    transaction
        .execute("DELETE FROM memory_fts WHERE memory_id=$id;", params![memory_id])
        .map_err(map_memory_write_error)?;
    let keyword_source = request
        .keywords
        .iter()
        .chain(request.tags.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    transaction
        .execute(
            "INSERT INTO memory_fts(memory_id,title_tokens,keyword_tokens,summary_tokens,content_tokens) \
             VALUES($id,$title,$keywords,$summary,$content);",
            params![
                memory_id,
                tokenize(&request.title),
                tokenize(&keyword_source),
                tokenize(&request.summary),
                tokenize(&request.content),
            ],
        )
        .map_err(map_memory_write_error)?;
    Ok(())
}

/// 删除不应继续云端处理的向量。
fn delete_embedding(transaction: &Transaction<'_>, memory_id: &str) -> Result<(), BusinessError> {
    transaction
        .execute("DELETE FROM memory_embedding WHERE memory_id=$id;", params![memory_id])
        .map_err(map_memory_write_error)?;
    Ok(())
}

/// 判断是否修改了需要进入正文历史的业务字段，收藏与置顶不生成历史版本。
fn has_versioned_changes(current: &MemoryItem, request: &SaveMemoryRequest) -> bool {
    current.scope != request.scope
        || current.project_id != request.project_id
        || current.title != request.title.trim()
        || current.summary != request.summary.trim()
        || current.content != request.content.trim()
        || current.memory_type != request.memory_type.trim()
        || current.keywords != normalize_list(&request.keywords)
        || current.tags != normalize_list(&request.tags)
        || current.importance != request.importance
        || current.cloud_processing_allowed != request.cloud_processing_allowed
}

/// 将当前记忆转换为不携带乐观锁版本的快照请求（字段序与 C# 序列化一致）。
fn create_snapshot_request(current: &MemoryItem) -> SaveMemoryRequest {
    SaveMemoryRequest {
        scope: current.scope,
        project_id: current.project_id.clone(),
        title: current.title.clone(),
        summary: current.summary.clone(),
        content: current.content.clone(),
        memory_type: current.memory_type.clone(),
        keywords: current.keywords.clone(),
        tags: current.tags.clone(),
        importance: current.importance,
        is_favorite: current.is_favorite,
        is_pinned: current.is_pinned,
        cloud_processing_allowed: current.cloud_processing_allowed,
        expected_version: None,
    }
}

/// 按 C# `JavaScriptEncoder.Default` 语义序列化字符串数组。
///
/// 非 ASCII 字符转 `\uXXXX`（大写十六进制），`< > & '` 同样转义——
/// 与 C# `JsonSerializerDefaults.Web` 写入 `keywords_json`/`tags_json` 的字节
/// 逐字一致。这决定了 LIKE 过滤（检索 tag 过滤、模糊回退 keywords）的跨语言
/// 行为一致：C# 侧 CJK tag 的 LIKE 永不命中（转义存储），Rust 必须同构。
pub(crate) fn serialize_string_list_csharp(values: &[String]) -> String {
    let mut output = String::with_capacity(values.len() * 8 + 2);
    output.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        for character in value.chars() {
            match character {
                '"' => output.push_str("\\\""),
                '\\' => output.push_str("\\\\"),
                '\u{08}' => output.push_str("\\b"),
                '\u{0C}' => output.push_str("\\f"),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                '<' => output.push_str("\\u003C"),
                '>' => output.push_str("\\u003E"),
                '&' => output.push_str("\\u0026"),
                '\'' => output.push_str("\\u0027"),
                character if (character as u32) < 0x20 => {
                    output.push_str(&format!("\\u{:04X}", character as u32));
                }
                character if (character as u32) <= 0x7F => output.push(character),
                character => {
                    let mut units = [0u16; 2];
                    for unit in character.encode_utf16(&mut units) {
                        output.push_str(&format!("\\u{unit:04X}"));
                    }
                }
            }
        }
        output.push('"');
    }
    output.push(']');
    output
}

/// 校验记忆业务输入（长度按 UTF-16 code unit，与 C# `Validate` 一致）。
fn validate_memory(request: &SaveMemoryRequest, require_version: bool) -> Result<(), BusinessError> {
    let utf16_length = |value: &str| value.chars().map(|c| c.len_utf16()).sum::<usize>();
    if request.title.trim().is_empty() || utf16_length(request.title.trim()) > 200 {
        return Err(BusinessError::new(ErrorCode::MemoryTitleInvalid));
    }
    if request.content.trim().is_empty() || utf16_length(&request.content) > 200000 {
        return Err(BusinessError::new(ErrorCode::MemoryContentInvalid));
    }
    if !(1..=5).contains(&request.importance) {
        return Err(BusinessError::new(ErrorCode::MemoryImportanceInvalid));
    }
    if !is_valid_memory_type(&request.memory_type) {
        return Err(BusinessError::new(ErrorCode::MemoryTypeInvalid));
    }
    if require_version && request.expected_version.is_none() {
        return Err(BusinessError::new(ErrorCode::MemoryVersionRequired));
    }
    Ok(())
}

/// 校验项目范围和项目归档状态。
fn validate_project(
    connection: &Connection,
    scope: MemoryScope,
    project_id: Option<&str>,
) -> Result<(), BusinessError> {
    if scope == MemoryScope::Personal && project_id.is_some() {
        return Err(BusinessError::new(ErrorCode::MemoryScopeInvalid));
    }
    if scope == MemoryScope::Project && project_id.is_none() {
        return Err(BusinessError::new(ErrorCode::MemoryProjectRequired));
    }
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let archived: Option<i64> = connection
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
    match archived {
        None => Err(BusinessError::new(ErrorCode::ProjectNotFound)),
        Some(value) if value != 0 => Err(BusinessError::new(ErrorCode::ProjectArchived)),
        Some(_) => Ok(()),
    }
}

/// 标准化标签或关键词列表（trim、去空、忽略大小写去重、最多 30 项，保留首见形态）。
pub(crate) fn normalize_list(values: &[String]) -> Vec<String> {
    let mut output: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) && output.len() < 30 {
            output.push(trimmed.to_string());
        }
    }
    output
}

/// 规范化可信来源名称，禁止写入空来源。
pub(crate) fn normalize_source(source_name: &str) -> Result<String, BusinessError> {
    let normalized = source_name.trim();
    if normalized.is_empty() {
        return Err(BusinessError::new(ErrorCode::MemorySourceRequired));
    }
    Ok(normalized.to_string())
}

/// 文本参数绑定。
fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_string())
}

/// INSERT 记忆参数（与 C# `AddMemoryParameters` 一致）。
fn memory_insert_params<'a>(
    id: &'a str,
    request: &'a SaveMemoryRequest,
    checksum: &'a str,
    source_name: &'a str,
    now_text: &'a str,
) -> Vec<(&'static str, SqlValue)> {
    vec![
        ("$id", text(id)),
        ("$scope", text(request.scope.as_scope_text())),
        ("$project_id", request.project_id.clone().into()),
        ("$title", text(request.title.trim())),
        ("$summary", text(request.summary.trim())),
        ("$content", text(request.content.trim())),
        ("$memory_type", text(request.memory_type.trim())),
        (
            "$keywords_json",
            serialize_string_list_csharp(&normalize_list(&request.keywords)).into(),
        ),
        (
            "$tags_json",
            serialize_string_list_csharp(&normalize_list(&request.tags)).into(),
        ),
        ("$importance", request.importance.into()),
        ("$is_favorite", (request.is_favorite as i64).into()),
        ("$is_pinned", (request.is_pinned as i64).into()),
        (
            "$cloud_processing_allowed",
            (request.cloud_processing_allowed as i64).into(),
        ),
        ("$checksum", text(checksum)),
        ("$source", text(&normalize_source(source_name).unwrap())),
        ("$created_at", text(now_text)),
        ("$updated_at", text(now_text)),
    ]
}

/// UPDATE 记忆参数（乐观锁双保险的 WHERE 版本条件）。
fn memory_update_params<'a>(
    id: &'a str,
    request: &'a SaveMemoryRequest,
    checksum: &'a str,
    now_text: &'a str,
    next_version: i64,
    expected_version: i64,
    updated_source: &'a str,
) -> Vec<(&'static str, SqlValue)> {
    vec![
        ("$scope", text(request.scope.as_scope_text())),
        ("$project_id", request.project_id.clone().into()),
        ("$title", text(request.title.trim())),
        ("$summary", text(request.summary.trim())),
        ("$content", text(request.content.trim())),
        ("$memory_type", text(request.memory_type.trim())),
        (
            "$keywords_json",
            serialize_string_list_csharp(&normalize_list(&request.keywords)).into(),
        ),
        (
            "$tags_json",
            serialize_string_list_csharp(&normalize_list(&request.tags)).into(),
        ),
        ("$importance", request.importance.into()),
        ("$is_favorite", (request.is_favorite as i64).into()),
        ("$is_pinned", (request.is_pinned as i64).into()),
        (
            "$cloud_processing_allowed",
            (request.cloud_processing_allowed as i64).into(),
        ),
        ("$next_version", next_version.into()),
        ("$checksum", text(checksum)),
        ("$updated_source", text(updated_source)),
        ("$updated_at", text(now_text)),
        ("$id", text(id)),
        ("$expected_version", expected_version.into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::FixedIdGenerator;
    use chrono::{DateTime, TimeZone, Utc};

    /// 构造测试服务：固定时钟顺序推进（每次调用 +1 秒），ID 顺序分配。
    struct TestContext {
        service: MemoryService,
        /// 持有临时目录直至测试结束（下划线前缀避免 dead_code 警告）。
        _temp: tempfile::TempDir,
    }

    /// 每次读取时推进 1 秒的固定时钟（保证时间戳单调，模拟真实写入）。
    struct StepClock {
        start: DateTime<Utc>,
        step: std::sync::atomic::AtomicI64,
    }

    impl Clock for StepClock {
        fn now_utc(&self) -> DateTime<Utc> {
            let step = self.step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.start + chrono::Duration::seconds(step)
        }
    }

    fn context() -> TestContext {
        let temp = tempfile::tempdir().unwrap();
        drop(memory_storage::open_initialized(&temp.path().join("test.db")).unwrap());
        let clock = Arc::new(StepClock {
            start: Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap(),
            step: std::sync::atomic::AtomicI64::new(0),
        });
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=12)
                .map(|index| format!("{index:08}-{index:04}-4{index:03}-8{index:03}-{index:012}"))
                .collect(),
        ));
        let service = MemoryService::new(Database::new(temp.path().join("test.db")), clock, ids);
        TestContext { service, _temp: temp }
    }

    fn personal_request(content: &str) -> SaveMemoryRequest {
        SaveMemoryRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title: "测试标题".to_string(),
            summary: "摘要".to_string(),
            content: content.to_string(),
            memory_type: "NOTE".to_string(),
            keywords: vec!["关键词".to_string()],
            tags: vec![],
            importance: 3,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: false,
            expected_version: None,
        }
    }

    fn query(size: i64, cursor: Option<String>) -> MemoryListQuery {
        MemoryListQuery {
            scope: None,
            project_id: None,
            status: Some(MemoryStatus::Active),
            is_favorite: None,
            is_pinned: None,
            memory_type: None,
            tag: None,
            importance_min: None,
            cursor,
            size,
        }
    }

    #[test]
    fn create_syncs_fts_and_returns_item() {
        let context = context();
        let memory = context.service.create(&personal_request("第一段内容")).unwrap();
        assert_eq!(memory.version, 1);
        assert_eq!(memory.status, MemoryStatus::Active);
        assert_eq!(memory.created_source, DESKTOP_SOURCE);
        assert_eq!(memory.created_at, "2026-08-15T08:00:00.0000000+00:00");
        // 与 C# 一致：create 用同一个 now，created_at == updated_at。
        assert_eq!(memory.updated_at, memory.created_at);
        // FTS 已写入。
        let connection = context.service.database.open().unwrap();
        let fts_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM memory_fts WHERE memory_id=$1;",
                params![memory.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fts_count, 1);
    }

    #[test]
    fn duplicate_content_raises_memory_duplicate() {
        let context = context();
        context.service.create(&personal_request("重复内容")).unwrap();
        let error = context.service.create(&personal_request(" 重复内容 ")).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryDuplicate);
    }

    #[test]
    fn cloud_allowed_queues_embedding_task() {
        let context = context();
        let mut request = personal_request("需要向量");
        request.cloud_processing_allowed = true;
        let memory = context.service.create(&request).unwrap();
        let connection = context.service.database.open().unwrap();
        let pending: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='EMBED_MEMORY' AND status='PENDING';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
        // 重复入队去重：更新后仍只有一条活动任务。
        let mut updated = request.clone();
        updated.expected_version = Some(memory.version);
        updated.content = "需要向量变体".to_string();
        context.service.update(&memory.id, &updated).unwrap();
        let pending_after: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='EMBED_MEMORY' AND status='PENDING';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending_after, 1);
    }

    #[test]
    fn favorite_only_update_does_not_bump_version_or_revision() {
        let context = context();
        let memory = context.service.create(&personal_request("正文")).unwrap();
        let mut request = personal_request("正文");
        request.is_favorite = true;
        request.expected_version = Some(memory.version);
        let updated = context.service.update(&memory.id, &request).unwrap();
        assert_eq!(updated.version, memory.version, "收藏不升版");
        assert!(updated.is_favorite);
        let revisions = context.service.list_revisions(&memory.id).unwrap();
        assert!(revisions.is_empty(), "收藏不产生历史版本");
    }

    #[test]
    fn content_change_bumps_version_and_replaces_revision() {
        let context = context();
        let memory = context.service.create(&personal_request("第一版")).unwrap();
        let mut request = personal_request("第二版");
        request.expected_version = Some(memory.version);
        let updated = context.service.update(&memory.id, &request).unwrap();
        assert_eq!(updated.version, 2);
        // 仅保留上一版快照（v1）。
        let revisions = context.service.list_revisions(&memory.id).unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].version, 1);
        // 再改一次：revision 被替换为 v2 快照（仍是仅一条）。
        let mut third = personal_request("第三版");
        third.expected_version = Some(updated.version);
        let updated3 = context.service.update(&memory.id, &third).unwrap();
        assert_eq!(updated3.version, 3);
        let revisions = context.service.list_revisions(&memory.id).unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].version, 2);
    }

    #[test]
    fn version_conflict_detected_both_ways() {
        let context = context();
        let memory = context.service.create(&personal_request("正文")).unwrap();
        // 应用层：期望版本不匹配。
        let mut stale = personal_request("新内容");
        stale.expected_version = Some(memory.version + 5);
        let error = context.service.update(&memory.id, &stale).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryVersionConflict);
        // 缺版本号。
        let error = context
            .service
            .update(&memory.id, &personal_request("新内容"))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryVersionRequired);
    }

    #[test]
    fn restore_blocked_while_project_archived() {
        let context = context();
        // 直插一个活动项目行（create 校验项目活动；restore 校验只读 project 表）。
        let connection = context.service.database.open().unwrap();
        connection
            .execute(
                "INSERT INTO project (id, name, description, color, is_archived, created_at, updated_at) \
                 VALUES ('11111111-1111-4111-8111-111111111111', '已归档项目', '', '#111111', 0, \
                 '2026-08-15T08:00:00.0000000+00:00', '2026-08-15T08:00:00.0000000+00:00');",
                [],
            )
            .unwrap();
        drop(connection);
        let mut request = personal_request("项目记忆");
        request.scope = MemoryScope::Project;
        request.project_id = Some("11111111-1111-4111-8111-111111111111".to_string());
        let memory = context.service.create(&request).unwrap();
        context.service.archive(&memory.id).unwrap();
        // 项目归档中：恢复被拦截并给出指引。
        let connection = context.service.database.open().unwrap();
        connection
            .execute(
                "UPDATE project SET is_archived=1 WHERE id='11111111-1111-4111-8111-111111111111';",
                [],
            )
            .unwrap();
        drop(connection);
        let error = context.service.restore(&memory.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectArchived);
        assert!(error.message.contains("已归档项目"));
        // 项目恢复活动后记忆可恢复。
        let connection = context.service.database.open().unwrap();
        connection
            .execute(
                "UPDATE project SET is_archived=0 WHERE id='11111111-1111-4111-8111-111111111111';",
                [],
            )
            .unwrap();
        drop(connection);
        let restored = context.service.restore(&memory.id).unwrap();
        assert_eq!(restored.status, MemoryStatus::Active);
    }

    #[test]
    fn archive_restore_delete_permanently_flow() {
        let context = context();
        let memory = context.service.create(&personal_request("待删除")).unwrap();
        // Active 不能直接彻底删除。
        let error = context.service.delete_permanently(&memory.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryNotArchived);
        // 归档 → 恢复 → 归档 → 彻底删除。
        let archived = context.service.archive(&memory.id).unwrap();
        assert_eq!(archived.status, MemoryStatus::Archived);
        assert!(archived.archived_at.is_some());
        // 归档记忆只读。
        let mut request = personal_request("改内容");
        request.expected_version = Some(archived.version);
        let error = context.service.update(&memory.id, &request).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryArchivedReadOnly);
        let restored = context.service.restore(&memory.id).unwrap();
        assert_eq!(restored.status, MemoryStatus::Active);
        assert!(restored.archived_at.is_none());
        context.service.archive(&memory.id).unwrap();
        context.service.delete_permanently(&memory.id).unwrap();
        let error = context.service.get(&memory.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryNotFound);
    }

    #[test]
    fn cursor_page_and_pinned_ordering() {
        let context = context();
        for index in 0..5 {
            let mut request = personal_request(&format!("内容{index}"));
            request.is_pinned = index == 4;
            context.service.create(&request).unwrap();
        }
        let page1 = context.service.list(&query(2, None)).unwrap();
        assert_eq!(page1.items.len(), 2);
        assert!(page1.has_more);
        assert!(page1.items[0].is_pinned, "置顶优先");
        assert!(page1.next_cursor.is_some());

        let page2 = context.service.list(&query(2, page1.next_cursor.clone())).unwrap();
        assert_eq!(page2.items.len(), 2);
        // 无重叠。
        assert_ne!(page1.items[0].id, page2.items[0].id);
        assert_ne!(page1.items[1].id, page2.items[1].id);
        // 全部取回后无更多。
        let mut cursor = page2.next_cursor.clone();
        let page3 = context.service.list(&query(10, cursor.clone())).unwrap();
        assert_eq!(page3.items.len(), 1);
        assert!(!page3.has_more);
        cursor = page3.next_cursor;
        assert!(cursor.is_none());
        // 总计 5 条不重复。
        let mut ids: Vec<String> = page1.items.iter().map(|m| m.id.clone()).collect();
        ids.extend(page2.items.iter().map(|m| m.id.clone()));
        ids.extend(page3.items.iter().map(|m| m.id.clone()));
        let unique: std::collections::HashSet<_> = ids.into_iter().collect();
        assert_eq!(unique.len(), 5);
    }

    #[test]
    fn restore_revision_roundtrip() {
        let context = context();
        let memory = context.service.create(&personal_request("第一版")).unwrap();
        let mut request = personal_request("第二版");
        request.expected_version = Some(memory.version);
        let updated = context.service.update(&memory.id, &request).unwrap();
        assert_eq!(updated.version, 2);
        // 恢复 v1：当前 v2 成为新快照，内容回到 v1。
        let restored = context.service.restore_revision(&memory.id, 1).unwrap();
        assert_eq!(restored.content, "第一版");
        assert_eq!(restored.version, 3);
        let revisions = context.service.list_revisions(&memory.id).unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].version, 2);
        // 版本不存在。
        let error = context.service.restore_revision(&memory.id, 99).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryRevisionNotFound);
    }

    #[test]
    fn quick_capture_generates_title_from_first_line() {
        let context = context();
        let memory = context
            .service
            .quick_capture(&QuickCaptureRequest {
                content: "第一行标题\n第二行内容".to_string(),
            })
            .unwrap();
        assert_eq!(memory.title, "第一行标题");
        assert_eq!(memory.scope, MemoryScope::Personal);
        assert_eq!(memory.memory_type, "NOTE");
        assert_eq!(memory.importance, 3);
        assert!(memory.cloud_processing_allowed);
        // 空内容。
        let error = context
            .service
            .quick_capture(&QuickCaptureRequest {
                content: "   ".to_string(),
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryContentRequired);
        // 超长标题截断到 60 个字符。
        let long_first_line = "标".repeat(61);
        let memory = context
            .service
            .quick_capture(&QuickCaptureRequest {
                content: format!("{long_first_line}\n正文"),
            })
            .unwrap();
        assert_eq!(memory.title.chars().count(), 60);
    }

    #[test]
    fn project_scope_validation() {
        let context = context();
        // Project 记忆缺项目。
        let mut request = personal_request("正文");
        request.scope = MemoryScope::Project;
        let error = context.service.create(&request).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryProjectRequired);
        // Personal 记忆带项目。
        let mut request = personal_request("正文");
        request.project_id = Some("00000000-0000-4000-8000-000000000009".to_string());
        let error = context.service.create(&request).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryScopeInvalid);
        // 项目不存在。
        let mut request = personal_request("正文");
        request.scope = MemoryScope::Project;
        request.project_id = Some("00000000-0000-4000-8000-000000000009".to_string());
        let error = context.service.create(&request).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectNotFound);
    }

    #[test]
    fn facets_aggregate_counts() {
        let context = context();
        context.service.create(&personal_request("A")).unwrap();
        let mut favorite = personal_request("B");
        favorite.is_favorite = true;
        context.service.create(&favorite).unwrap();
        context
            .service
            .quick_capture(&QuickCaptureRequest {
                content: "快速记录内容".to_string(),
            })
            .unwrap();
        let facets = context.service.get_facets().unwrap();
        assert_eq!(facets.all_count, 3);
        assert_eq!(facets.personal_count, 3);
        assert_eq!(facets.favorite_count, 1);
        assert_eq!(facets.pinned_count, 0);
        assert_eq!(facets.archived_count, 0);
    }

    #[test]
    fn normalize_list_dedupes_case_insensitive_and_keeps_first() {
        assert_eq!(
            normalize_list(&[
                "Apple".to_string(),
                " apple ".to_string(),
                "Banana".to_string(),
                "".to_string()
            ]),
            vec!["Apple".to_string(), "Banana".to_string()]
        );
        let many: Vec<String> = (0..40).map(|index| index.to_string()).collect();
        assert_eq!(normalize_list(&many).len(), 30);
    }
}
