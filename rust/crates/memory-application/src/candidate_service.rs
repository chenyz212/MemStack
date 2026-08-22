//! 候选记忆服务：移植 C# `MemoryCandidateService`。
//!
//! 候选确认前完全隔离于正式索引：submit/update/get/list 只触碰
//! `memory_candidate` 表（不写 memory/FTS/background_task）；
//! confirm 委托 `MemoryService::confirm_candidate`（同一事务写入正式记忆并删候选）。

use std::sync::Arc;

use memory_domain::{
    BusinessError, ErrorCode, McpCallerContext, MemoryCandidateItem, MemoryItem, MemoryScope,
    SaveMemoryCandidateRequest, content_checksum, is_valid_memory_type,
};
use rusqlite::{Connection, params};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::mcp_access::{require_project_scope, require_write};
use crate::memory_service::{MemoryService, serialize_string_list_csharp};
use crate::sqlite_errors::{map_sqlite_error, unique_constraint_error};

/// 候选行读取 SQL（与 C# `SelectCandidateSql` 一致）。
const SELECT_CANDIDATE_SQL: &str = "
    SELECT c.id,c.scope,c.project_id,p.name,c.title,c.summary,c.content,c.memory_type, \
           c.keywords_json,c.tags_json,c.importance,c.cloud_processing_allowed, \
           c.source_name,c.version,c.created_at,c.updated_at \
    FROM memory_candidate c LEFT JOIN project p ON p.id=c.project_id";

/// 管理确认前完全隔离于正式索引的候选记忆。
pub struct MemoryCandidateService {
    database: Database,
    memories: Arc<MemoryService>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl MemoryCandidateService {
    /// 创建候选记忆服务。
    pub fn new(
        database: Database,
        memories: Arc<MemoryService>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            database,
            memories,
            clock,
            ids,
        }
    }

    /// 提交候选，不触碰正式记忆、全文索引和向量任务。
    pub fn submit(
        &self,
        request: &SaveMemoryCandidateRequest,
        source_name: &str,
    ) -> Result<MemoryCandidateItem, BusinessError> {
        validate_candidate(request, false)?;
        let id = self.ids.new_id();
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        let result = connection.execute(
            "INSERT INTO memory_candidate( \
                 id,scope,project_id,title,summary,content,status,created_at,updated_at, \
                 memory_type,keywords_json,tags_json,importance,cloud_processing_allowed, \
                 content_checksum,source_name,version) \
             VALUES( \
                 $id,$scope,$project_id,$title,$summary,$content,'PENDING',$created_at,$updated_at, \
                 $memory_type,$keywords_json,$tags_json,$importance,$cloud_processing_allowed, \
                 $checksum,$source_name,1);",
            params![
                id,
                request.scope.as_scope_text(),
                request.project_id,
                request.title.trim(),
                request.summary.trim(),
                request.content.trim(),
                now_text,
                now_text,
                request.memory_type.trim(),
                serialize_string_list_csharp(&normalize_candidate_list(&request.keywords)),
                serialize_string_list_csharp(&normalize_candidate_list(&request.tags)),
                request.importance,
                request.cloud_processing_allowed as i64,
                content_checksum(&request.content),
                source_name.trim(),
            ],
        );
        match result {
            Ok(_) => {}
            Err(error) => {
                return if unique_constraint_error(&error).is_some() {
                    Err(BusinessError::new(ErrorCode::MemoryCandidateDuplicate))
                } else {
                    Err(map_sqlite_error(error))
                };
            }
        }
        get_candidate(&connection, &id)
    }

    /// 原子提交结论卡片候选：普通候选字段与结构化载荷在同一条语句中落库。
    pub(crate) fn submit_conclusion(
        &self,
        request: &SaveMemoryCandidateRequest,
        source_name: &str,
        structured_payload_json: &str,
    ) -> Result<MemoryCandidateItem, BusinessError> {
        validate_candidate(request, false)?;
        let id = self.ids.new_id();
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        let result = connection.execute(
            "INSERT INTO memory_candidate( \
                 id,scope,project_id,title,summary,content,status,created_at,updated_at, \
                 memory_type,keywords_json,tags_json,importance,cloud_processing_allowed, \
                 content_checksum,source_name,version,structured_payload_json) \
             VALUES( \
                 $id,$scope,$project_id,$title,$summary,$content,'PENDING',$created_at,$updated_at, \
                 $memory_type,$keywords_json,$tags_json,$importance,$cloud_processing_allowed, \
                 $checksum,$source_name,1,$structured_payload_json);",
            params![
                id,
                request.scope.as_scope_text(),
                request.project_id,
                request.title.trim(),
                request.summary.trim(),
                request.content.trim(),
                now_text,
                now_text,
                request.memory_type.trim(),
                serialize_string_list_csharp(&normalize_candidate_list(&request.keywords)),
                serialize_string_list_csharp(&normalize_candidate_list(&request.tags)),
                request.importance,
                request.cloud_processing_allowed as i64,
                content_checksum(&request.content),
                source_name.trim(),
                structured_payload_json,
            ],
        );
        match result {
            Ok(_) => {}
            Err(error) => {
                return if unique_constraint_error(&error).is_some() {
                    Err(BusinessError::new(ErrorCode::MemoryCandidateDuplicate))
                } else {
                    Err(map_sqlite_error(error))
                };
            }
        }
        get_candidate(&connection, &id)
    }

    /// 按可信 Token 范围列出待确认候选。
    pub fn list(&self, caller: &McpCallerContext) -> Result<Vec<MemoryCandidateItem>, BusinessError> {
        let connection = self.database.open()?;
        let sql = format!(
            "{SELECT_CANDIDATE_SQL} \
             WHERE c.status='PENDING' \
               AND ($project_id IS NULL OR c.project_id=$project_id) \
             ORDER BY c.updated_at DESC,c.id DESC;"
        );
        let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
        let rows = statement
            .query_map(params![caller.project_id], read_candidate)
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(rows)
    }

    /// 编辑候选的全部业务字段并执行乐观锁。
    pub fn update(&self, id: &str, request: &SaveMemoryCandidateRequest) -> Result<MemoryCandidateItem, BusinessError> {
        validate_candidate(request, true)?;
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        let current = get_candidate(&connection, id)?;
        let changed = connection
            .execute(
                "UPDATE memory_candidate \
                 SET scope=$scope,project_id=$project_id,title=$title,summary=$summary,content=$content, \
                     memory_type=$memory_type,keywords_json=$keywords_json,tags_json=$tags_json, \
                     importance=$importance,cloud_processing_allowed=$cloud_processing_allowed, \
                     content_checksum=$checksum,version=version+1,updated_at=$updated_at \
                 WHERE id=$id AND status='PENDING' AND version=$expected_version;",
                params![
                    request.scope.as_scope_text(),
                    request.project_id,
                    request.title.trim(),
                    request.summary.trim(),
                    request.content.trim(),
                    request.memory_type.trim(),
                    serde_json::to_string(&normalize_candidate_list(&request.keywords)).unwrap(),
                    serde_json::to_string(&normalize_candidate_list(&request.tags)).unwrap(),
                    request.importance,
                    request.cloud_processing_allowed as i64,
                    content_checksum(&request.content),
                    now_text,
                    id,
                    request.expected_version.unwrap_or(current.version),
                ],
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            return Err(BusinessError::new(ErrorCode::MemoryCandidateVersionConflict));
        }
        get_candidate(&connection, id)
    }

    /// 原子更新结论卡片候选：业务字段、结构化载荷与版本号保持一致。
    pub(crate) fn update_conclusion(
        &self,
        id: &str,
        request: &SaveMemoryCandidateRequest,
        structured_payload_json: &str,
    ) -> Result<MemoryCandidateItem, BusinessError> {
        validate_candidate(request, true)?;
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        let current = get_candidate(&connection, id)?;
        let changed = connection
            .execute(
                "UPDATE memory_candidate \
                 SET scope=$scope,project_id=$project_id,title=$title,summary=$summary,content=$content, \
                     memory_type=$memory_type,keywords_json=$keywords_json,tags_json=$tags_json, \
                     importance=$importance,cloud_processing_allowed=$cloud_processing_allowed, \
                     content_checksum=$checksum,structured_payload_json=$structured_payload_json, \
                     version=version+1,updated_at=$updated_at \
                 WHERE id=$id AND status='PENDING' AND version=$expected_version;",
                params![
                    request.scope.as_scope_text(),
                    request.project_id,
                    request.title.trim(),
                    request.summary.trim(),
                    request.content.trim(),
                    request.memory_type.trim(),
                    serde_json::to_string(&normalize_candidate_list(&request.keywords)).unwrap(),
                    serde_json::to_string(&normalize_candidate_list(&request.tags)).unwrap(),
                    request.importance,
                    request.cloud_processing_allowed as i64,
                    content_checksum(&request.content),
                    structured_payload_json,
                    now_text,
                    id,
                    request.expected_version.unwrap_or(current.version),
                ],
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            return Err(BusinessError::new(ErrorCode::MemoryCandidateVersionConflict));
        }
        get_candidate(&connection, id)
    }

    /// 确认候选并生成正式记忆；重复冲突时保留候选（事务回滚）。
    ///
    /// 结论卡片候选（structured_payload_json 非空）在同一事务内写入 conclusion_card 扩展。
    pub fn confirm(
        &self,
        id: &str,
        expected_version: i64,
        caller: &McpCallerContext,
    ) -> Result<MemoryItem, BusinessError> {
        require_write(caller)?;
        let connection = self.database.open()?;
        let candidate = get_candidate(&connection, id)?;
        require_project_scope(caller, candidate.scope, candidate.project_id.as_deref())?;
        if candidate.version != expected_version {
            return Err(BusinessError::new(ErrorCode::MemoryCandidateVersionConflict));
        }
        let conclusion_payload = read_conclusion_payload(&connection, id)?;
        let save_request = memory_domain::SaveMemoryRequest {
            scope: candidate.scope,
            project_id: candidate.project_id.clone(),
            title: candidate.title.clone(),
            summary: candidate.summary.clone(),
            content: candidate.content.clone(),
            memory_type: candidate.memory_type.clone(),
            keywords: candidate.keywords.clone(),
            tags: candidate.tags.clone(),
            importance: candidate.importance,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: candidate.cloud_processing_allowed,
            expected_version: None,
        };
        self.memories.confirm_candidate(
            id,
            expected_version,
            &save_request,
            &candidate.source_name,
            conclusion_payload.as_ref(),
        )
    }

    /// 使用乐观锁拒绝并删除候选。
    pub fn reject(&self, id: &str, expected_version: i64, caller: &McpCallerContext) -> Result<(), BusinessError> {
        require_write(caller)?;
        let connection = self.database.open()?;
        let candidate = get_candidate(&connection, id)?;
        require_project_scope(caller, candidate.scope, candidate.project_id.as_deref())?;
        let deleted = connection
            .execute(
                "DELETE FROM memory_candidate WHERE id=$id AND status='PENDING' AND version=$version;",
                params![id, expected_version],
            )
            .map_err(map_sqlite_error)?;
        if deleted == 0 {
            return Err(BusinessError::new(ErrorCode::MemoryCandidateVersionConflict));
        }
        Ok(())
    }

    /// 读取指定候选。
    pub fn get(&self, id: &str) -> Result<MemoryCandidateItem, BusinessError> {
        let connection = self.database.open()?;
        get_candidate(&connection, id)
    }
}

/// 使用已有连接读取指定候选（仅 PENDING）。
pub(crate) fn get_candidate(connection: &Connection, id: &str) -> Result<MemoryCandidateItem, BusinessError> {
    let sql = format!("{SELECT_CANDIDATE_SQL} WHERE c.id=$id AND c.status='PENDING';");
    connection
        .query_row(&sql, params![id], read_candidate)
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => BusinessError::new(ErrorCode::MemoryCandidateNotFound),
            other => map_sqlite_error(other),
        })
}

/// 读取候选上挂载的结论卡片结构化数据（普通候选返回 None）。
pub(crate) fn read_conclusion_payload(
    connection: &Connection,
    id: &str,
) -> Result<Option<memory_domain::ConclusionCardPayload>, BusinessError> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT structured_payload_json FROM memory_candidate WHERE id=$id;",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(map_sqlite_error)?;
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    serde_json::from_str(&raw).map(Some).map_err(|error| {
        BusinessError::with_message(ErrorCode::InternalError, format!("结论卡片候选数据损坏：{error}"))
    })
}

/// 校验候选业务字段和乐观锁参数（消息与 C# 逐字一致：均带「候选」前缀）。
fn validate_candidate(request: &SaveMemoryCandidateRequest, require_version: bool) -> Result<(), BusinessError> {
    let utf16_length = |value: &str| value.chars().map(|c| c.len_utf16()).sum::<usize>();
    if request.title.trim().is_empty() || utf16_length(request.title.trim()) > 200 {
        return Err(BusinessError::with_message(
            ErrorCode::MemoryTitleInvalid,
            "候选标题长度必须为 1 到 200 个字符",
        ));
    }
    if request.content.trim().is_empty() || utf16_length(&request.content) > 200000 {
        return Err(BusinessError::with_message(
            ErrorCode::MemoryContentInvalid,
            "候选正文长度必须为 1 到 200000 个字符",
        ));
    }
    if (request.scope == MemoryScope::Personal && request.project_id.is_some())
        || (request.scope == MemoryScope::Project && request.project_id.is_none())
    {
        return Err(BusinessError::with_message(
            ErrorCode::MemoryScopeInvalid,
            "候选范围与项目不匹配",
        ));
    }
    if !(1..=5).contains(&request.importance) {
        return Err(BusinessError::new(ErrorCode::MemoryImportanceInvalid));
    }
    if !is_valid_memory_type(&request.memory_type) {
        return Err(BusinessError::new(ErrorCode::MemoryTypeInvalid));
    }
    if require_version && request.expected_version.is_none() {
        return Err(BusinessError::new(ErrorCode::MemoryCandidateVersionRequired));
    }
    Ok(())
}

/// 标准化候选标签或关键词列表（与 MemoryService 同规则）。
fn normalize_candidate_list(values: &[String]) -> Vec<String> {
    crate::memory_service::normalize_list(values)
}

/// 从行构造完整候选记忆（与 C# `ReadCandidate` 一致）。
fn read_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryCandidateItem> {
    let scope_text: String = row.get(1)?;
    Ok(MemoryCandidateItem {
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
        keywords: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
        tags: serde_json::from_str(&row.get::<_, String>(9)?).unwrap_or_default(),
        importance: row.get(10)?,
        cloud_processing_allowed: row.get::<_, i64>(11)? != 0,
        source_name: row.get(12)?,
        version: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use crate::memory_service::MemoryService;
    use chrono::{TimeZone, Utc};
    use memory_domain::{McpPermission, SaveMemoryRequest};

    struct TestContext {
        candidates: MemoryCandidateService,
        memories: Arc<MemoryService>,
        database: Database,
        caller: McpCallerContext,
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
        let memories = Arc::new(MemoryService::new(database.clone(), clock.clone(), ids.clone()));
        let candidates = MemoryCandidateService::new(database.clone(), memories.clone(), clock, ids);
        let caller = McpCallerContext {
            token_id: "00000000-0000-4000-8000-000000000001".to_string(),
            display_name: "Codex".to_string(),
            permission: McpPermission::ReadWrite,
            project_id: None,
        };
        TestContext {
            candidates,
            memories,
            database,
            caller,
            _temp: temp,
        }
    }

    fn request(content: &str) -> SaveMemoryCandidateRequest {
        SaveMemoryCandidateRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title: "候选标题".to_string(),
            summary: "候选摘要".to_string(),
            content: content.to_string(),
            memory_type: "NOTE".to_string(),
            keywords: vec![],
            tags: vec![],
            importance: 3,
            cloud_processing_allowed: false,
            expected_version: None,
        }
    }

    #[test]
    fn submit_is_isolated_from_formal_memory() {
        let context = context();
        let candidate = context.candidates.submit(&request("候选内容A"), "Codex").unwrap();
        assert_eq!(candidate.version, 1);
        assert_eq!(candidate.source_name, "Codex");
        // 正式记忆 / FTS / 任务表完全未动。
        let connection = context.database.open().unwrap();
        for table in ["memory", "memory_fts", "background_task"] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table};"), [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} 不应被候选写入触碰");
        }
    }

    #[test]
    fn duplicate_submit_raises_candidate_duplicate() {
        let context = context();
        context.candidates.submit(&request("重复候选"), "Codex").unwrap();
        let error = context.candidates.submit(&request(" 重复候选 "), "Codex").unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateDuplicate);
    }

    #[test]
    fn update_bumps_version_with_optimistic_lock() {
        let context = context();
        let candidate = context.candidates.submit(&request("第一版"), "Codex").unwrap();
        let mut updated_request = request("第二版");
        updated_request.expected_version = Some(candidate.version);
        let updated = context.candidates.update(&candidate.id, &updated_request).unwrap();
        assert_eq!(updated.version, 2);
        assert_eq!(updated.content, "第二版");
        // 过期版本冲突。
        let error = context.candidates.update(&candidate.id, &updated_request).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateVersionConflict);
        // 缺版本号。
        let error = context
            .candidates
            .update(&candidate.id, &request("无版本"))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateVersionRequired);
    }

    #[test]
    fn confirm_creates_memory_and_deletes_candidate() {
        let context = context();
        let candidate = context.candidates.submit(&request("待确认"), "Codex").unwrap();
        let memory = context.candidates.confirm(&candidate.id, 1, &context.caller).unwrap();
        assert_eq!(memory.content, "待确认");
        assert_eq!(memory.created_source, "Codex");
        assert_eq!(memory.scope, MemoryScope::Personal);
        // 候选已删除。
        let error = context.candidates.get(&candidate.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateNotFound);
        // FTS 已同步。
        let connection = context.database.open().unwrap();
        let fts: i64 = connection
            .query_row("SELECT count(*) FROM memory_fts;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fts, 1);
    }

    #[test]
    fn confirm_with_stale_version_fails_and_keeps_candidate() {
        let context = context();
        let candidate = context.candidates.submit(&request("内容"), "Codex").unwrap();
        let error = context
            .candidates
            .confirm(&candidate.id, 5, &context.caller)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateVersionConflict);
        // 候选保留。
        assert!(context.candidates.get(&candidate.id).is_ok());
    }

    #[test]
    fn confirm_duplicate_content_rolls_back_candidate_deletion() {
        let context = context();
        // 先建一条同内容正式记忆。
        context
            .memories
            .create(&SaveMemoryRequest {
                scope: MemoryScope::Personal,
                project_id: None,
                title: "正式".to_string(),
                summary: String::new(),
                content: "冲突内容".to_string(),
                memory_type: "NOTE".to_string(),
                keywords: vec![],
                tags: vec![],
                importance: 3,
                is_favorite: false,
                is_pinned: false,
                cloud_processing_allowed: false,
                expected_version: None,
            })
            .unwrap();
        let candidate = context.candidates.submit(&request("冲突内容"), "Codex").unwrap();
        let error = context
            .candidates
            .confirm(&candidate.id, 1, &context.caller)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryDuplicate);
        // 事务回滚：候选仍在。
        assert!(context.candidates.get(&candidate.id).is_ok());
    }

    #[test]
    fn reject_deletes_with_optimistic_lock() {
        let context = context();
        let candidate = context.candidates.submit(&request("拒绝"), "Codex").unwrap();
        context.candidates.reject(&candidate.id, 1, &context.caller).unwrap();
        let error = context.candidates.get(&candidate.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateNotFound);
        // 版本不符。
        let candidate2 = context.candidates.submit(&request("再拒绝"), "Codex").unwrap();
        let error = context
            .candidates
            .reject(&candidate2.id, 9, &context.caller)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateVersionConflict);
    }

    #[test]
    fn read_only_caller_cannot_confirm_or_reject() {
        let context = context();
        let candidate = context.candidates.submit(&request("只读测试"), "Codex").unwrap();
        let reader = McpCallerContext {
            token_id: context.caller.token_id.clone(),
            display_name: context.caller.display_name.clone(),
            permission: McpPermission::Read,
            project_id: None,
        };
        let error = context.candidates.confirm(&candidate.id, 1, &reader).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpPermissionDenied);
        let error = context.candidates.reject(&candidate.id, 1, &reader).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpPermissionDenied);
    }

    #[test]
    fn list_filters_by_caller_project_scope() {
        let context = context();
        // 个人候选（全局 Token 可见）。
        context.candidates.submit(&request("个人候选"), "Codex").unwrap();
        // 项目候选。
        let connection = context.database.open().unwrap();
        connection
            .execute(
                "INSERT INTO project(id,name,description,color,is_archived,created_at,updated_at) \
                 VALUES('99999999-9999-4999-8999-999999999999','范围项目','','#123456',0,'2026-08-15T08:00:00.0000000+00:00','2026-08-15T08:00:00.0000000+00:00');",
                [],
            )
            .unwrap();
        drop(connection);
        let mut project_request = request("项目候选");
        project_request.scope = MemoryScope::Project;
        project_request.project_id = Some("99999999-9999-4999-8999-999999999999".to_string());
        context.candidates.submit(&project_request, "Codex").unwrap();

        // 全局 Token 看到全部。
        assert_eq!(context.candidates.list(&context.caller).unwrap().len(), 2);
        // 项目 Token 只看本项目的。
        let scoped = McpCallerContext {
            token_id: context.caller.token_id.clone(),
            display_name: context.caller.display_name.clone(),
            permission: McpPermission::ReadWrite,
            project_id: Some("99999999-9999-4999-8999-999999999999".to_string()),
        };
        let scoped_list = context.candidates.list(&scoped).unwrap();
        assert_eq!(scoped_list.len(), 1);
        assert_eq!(scoped_list[0].content, "项目候选");
    }
}
