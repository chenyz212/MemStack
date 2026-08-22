//! 结论卡片服务：结构化解决经验的候选提交、编辑与查询（执行计划 §7.4、§9.6、§17–§19）。
//!
//! - 候选阶段复用 memory_candidate（`structured_payload_json` 挂载结构化数据），
//!   确认前完全隔离于正式索引。
//! - 用户确认时由 `MemoryService::confirm_candidate` 在同一事务内创建 SOLUTION 记忆、
//!   conclusion_card 扩展、FTS、可选 Embedding 任务与图谱任务。
//! - 是否允许云端嵌入由卡片自身字段决定，不继承项目文档设置（§19.2）。

use std::sync::Arc;

use memory_domain::{
    BusinessError, ConclusionCardItem, ConclusionCardPayload, ErrorCode, MemoryCandidateItem, MemoryScope,
    SaveMemoryCandidateRequest,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::candidate_service::MemoryCandidateService;
use crate::db::Database;
use crate::sqlite_errors::map_sqlite_error;
use crate::workspace_identity::{calculate_key, normalize_identifier};

/// 结论卡片候选使用的固定记忆类型（§9.6：调用方不能借此创建其他记忆类型）。
pub const CONCLUSION_CARD_MEMORY_TYPE: &str = "SOLUTION";

/// 管理结论卡片候选的提交、编辑与正式卡片查询。
pub struct ConclusionCardService {
    database: Database,
    candidates: Arc<MemoryCandidateService>,
}

impl ConclusionCardService {
    /// 创建结论卡片应用服务。
    pub fn new(database: Database, candidates: Arc<MemoryCandidateService>) -> Self {
        Self { database, candidates }
    }

    /// 提交一张结论卡片候选（§9.6）：结构化数据 + 渲染 Markdown 正文。
    pub fn submit_candidate(
        &self,
        workspace_path: &str,
        payload: &ConclusionCardPayload,
        source_name: &str,
    ) -> Result<MemoryCandidateItem, BusinessError> {
        validate_payload(payload)?;
        let project_id = resolve_project_id_by_workspace(&self.database.open()?, workspace_path)?;
        let request = SaveMemoryCandidateRequest {
            scope: MemoryScope::Project,
            project_id: Some(project_id),
            title: payload.title.trim().to_string(),
            summary: build_summary(payload),
            content: render_markdown(payload),
            memory_type: CONCLUSION_CARD_MEMORY_TYPE.to_string(),
            keywords: payload.keywords.clone(),
            tags: payload.tags.clone(),
            importance: payload.importance,
            cloud_processing_allowed: payload.cloud_embedding_allowed,
            expected_version: None,
        };
        let payload_json = serde_json::to_string(payload).map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("序列化结论卡片失败：{error}"))
        })?;
        self.candidates.submit_conclusion(&request, source_name, &payload_json)
    }

    /// 读取候选上的结构化卡片数据（普通候选返回 None；供桌面端字段编辑）。
    pub fn get_candidate_payload(&self, candidate_id: &str) -> Result<Option<ConclusionCardPayload>, BusinessError> {
        let connection = self.database.open()?;
        crate::candidate_service::read_conclusion_payload(&connection, candidate_id)
    }

    /// 编辑候选阶段的结构化卡片：重新校验、重渲染正文并保持候选乐观锁（§21.4）。
    pub fn update_candidate_payload(
        &self,
        candidate_id: &str,
        payload: &ConclusionCardPayload,
        expected_version: i64,
    ) -> Result<MemoryCandidateItem, BusinessError> {
        validate_payload(payload)?;
        let current = self.candidates.get(candidate_id)?;
        if current.memory_type.to_uppercase() != CONCLUSION_CARD_MEMORY_TYPE {
            return Err(BusinessError::with_message(
                ErrorCode::MemoryTypeInvalid,
                "该候选不是结论卡片，不能使用结论卡片编辑",
            ));
        }
        let request = SaveMemoryCandidateRequest {
            scope: current.scope,
            project_id: current.project_id.clone(),
            title: payload.title.trim().to_string(),
            summary: build_summary(payload),
            content: render_markdown(payload),
            memory_type: current.memory_type.clone(),
            keywords: payload.keywords.clone(),
            tags: payload.tags.clone(),
            importance: payload.importance,
            cloud_processing_allowed: payload.cloud_embedding_allowed,
            expected_version: Some(expected_version),
        };
        let payload_json = serde_json::to_string(payload).map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("序列化结论卡片失败：{error}"))
        })?;
        self.candidates.update_conclusion(candidate_id, &request, &payload_json)
    }

    /// 读取一张正式结论卡片（确认后）。
    pub fn get_card(&self, memory_id: &str) -> Result<ConclusionCardItem, BusinessError> {
        let connection = self.database.open()?;
        let row = connection
            .query_row(
                "SELECT cc.problem_id,cc.structured_payload_json,cc.created_at,cc.updated_at \
                 FROM conclusion_card cc WHERE cc.memory_id=$id;",
                params![memory_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?
            .ok_or_else(|| BusinessError::new(ErrorCode::ConclusionCardNotFound))?;
        let payload: ConclusionCardPayload = serde_json::from_str(&row.1).map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("结论卡片数据损坏：{error}"))
        })?;
        Ok(ConclusionCardItem {
            memory_id: memory_id.to_string(),
            title: payload.title.clone(),
            problem_id: row.0,
            payload,
            created_at: row.2,
            updated_at: row.3,
        })
    }

    /// 列出项目全部正式结论卡片（按重要度倒序；供桌面端卡片列表）。
    pub fn list_cards_for_project(&self, project_id: &str) -> Result<Vec<ConclusionCardItem>, BusinessError> {
        let connection = self.database.open()?;
        let mut statement = connection
            .prepare(
                "SELECT cc.memory_id,cc.problem_id,cc.structured_payload_json,cc.created_at,cc.updated_at \
                 FROM conclusion_card cc INNER JOIN memory m ON m.id=cc.memory_id \
                 WHERE m.project_id=$pid AND m.status='Active' \
                 ORDER BY json_extract(cc.structured_payload_json,'$.importance') DESC, cc.updated_at DESC;",
            )
            .map_err(map_sqlite_error)?;
        let rows = statement
            .query_map(params![project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        rows.into_iter()
            .map(|(memory_id, problem_id, payload_json, created_at, updated_at)| {
                let payload: ConclusionCardPayload = serde_json::from_str(&payload_json).map_err(|error| {
                    BusinessError::with_message(ErrorCode::InternalError, format!("结论卡片数据损坏：{error}"))
                })?;
                Ok(ConclusionCardItem {
                    title: payload.title.clone(),
                    memory_id,
                    problem_id,
                    payload,
                    created_at,
                    updated_at,
                })
            })
            .collect()
    }
}

/// 按工作空间绝对路径解析绑定项目 ID。
fn resolve_project_id_by_workspace(connection: &Connection, workspace_path: &str) -> Result<String, BusinessError> {
    let identifier = normalize_identifier(workspace_path)?;
    let key = calculate_key(&identifier);
    connection
        .query_row(
            "SELECT id FROM project WHERE workspace_key=$key;",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite_error)?
        .ok_or_else(|| {
            BusinessError::with_message(
                ErrorCode::ProjectDocumentWorkspaceUnbound,
                format!("工作空间「{identifier}」尚未绑定 MemStack 项目，无法提交结论卡片"),
            )
        })
}

/// 校验结论卡片必填字段（§7.5）。
pub fn validate_payload(payload: &ConclusionCardPayload) -> Result<(), BusinessError> {
    let missing = |field: &str| {
        BusinessError::with_message(
            ErrorCode::ConclusionCardFieldRequired,
            format!("结论卡片缺少必填字段：{field}"),
        )
    };
    if payload.title.trim().is_empty() {
        return Err(missing("标题"));
    }
    if payload.problem_description.trim().is_empty() {
        return Err(missing("问题描述"));
    }
    if payload.final_conclusion.trim().is_empty() {
        return Err(missing("最终结论"));
    }
    if payload.root_cause.trim().is_empty() {
        return Err(missing("根本原因"));
    }
    if payload.evidence.iter().all(|item| item.trim().is_empty()) {
        return Err(missing("证据（至少一条）"));
    }
    if payload.verified_results.iter().all(|item| item.trim().is_empty()) {
        return Err(missing("已验证结果（至少一条）"));
    }
    if !(1..=5).contains(&payload.importance) {
        return Err(BusinessError::with_message(
            ErrorCode::ConclusionCardFieldRequired,
            "结论卡片重要度必须为 1 到 5",
        ));
    }
    if payload.importance_reason.trim().is_empty() {
        return Err(missing("重要度评估理由"));
    }
    if payload.resolved_at.trim().is_empty() {
        return Err(missing("解决时间"));
    }
    Ok(())
}

/// 候选摘要：问题描述 → 最终结论（限 200 字符）。
fn build_summary(payload: &ConclusionCardPayload) -> String {
    let summary = format!(
        "{} → {}",
        payload.problem_description.trim(),
        payload.final_conclusion.trim()
    );
    summary.chars().take(200).collect()
}

/// 把结构化卡片渲染为 Markdown 正文（候选与正式记忆共用同一渲染结果）。
pub fn render_markdown(payload: &ConclusionCardPayload) -> String {
    let list = |items: &[String]| -> String {
        if items.is_empty() {
            return "（无）\n".to_string();
        }
        items.iter().map(|item| format!("- {}\n", item.trim())).collect()
    };
    let problem = payload.problem_id.as_deref().unwrap_or("无").to_string();
    format!(
        "# {title}\n\n\
         - 关联问题：{problem}\n\
         - 解决时间：{resolved_at}\n\
         - 重要度：{importance}/5（{reason}）\n\
         - 云端嵌入：{embedding}\n\n\
         ## 问题描述\n\n{problem_description}\n\n\
         ## 最终结论\n\n{final_conclusion}\n\n\
         ## 根本原因\n\n{root_cause}\n\n\
         ## 适用条件\n\n{applicable}\n\
         ## 不适用条件\n\n{not_applicable}\n\
         ## 证据\n\n{evidence}\n\
         ## 已验证结果\n\n{verified}\n\
         ## 失败方案（不要重复）\n\n{failed}\n\
         ## 重新尝试条件\n\n{retry}\n\
         ## 下一步\n\n{next}\n",
        title = payload.title.trim(),
        problem = problem,
        resolved_at = payload.resolved_at.trim(),
        importance = payload.importance,
        reason = payload.importance_reason.trim(),
        embedding = if payload.cloud_embedding_allowed {
            "允许"
        } else {
            "禁止"
        },
        problem_description = payload.problem_description.trim(),
        final_conclusion = payload.final_conclusion.trim(),
        root_cause = payload.root_cause.trim(),
        applicable = list(&payload.applicable_conditions),
        not_applicable = list(&payload.not_applicable_conditions),
        evidence = list(&payload.evidence),
        verified = list(&payload.verified_results),
        failed = list(&payload.failed_attempts),
        retry = list(&payload.retry_conditions),
        next = list(&payload.next_steps),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::{FixedIdGenerator, GuidGenerator};
    use crate::memory_service::MemoryService;
    use chrono::{TimeZone, Utc};
    use memory_domain::{McpCallerContext, McpPermission, ProjectHandoffRequest};

    struct TestContext {
        cards: ConclusionCardService,
        candidates: Arc<MemoryCandidateService>,
        database: Database,
        workspace: tempfile::TempDir,
        /// 数据库所在临时目录（保持存活，防止目录被提前清理）。
        _directory: tempfile::TempDir,
        caller: McpCallerContext,
    }

    fn context() -> TestContext {
        let temp = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let database = Database::new(database_path);
        let clock = Arc::new(FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 21, 8, 0, 0).unwrap()));
        let ids = Arc::new(GuidGenerator);
        let memories = Arc::new(MemoryService::new(database.clone(), clock.clone(), ids.clone()));
        let candidates = Arc::new(MemoryCandidateService::new(
            database.clone(),
            memories.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let cards = ConclusionCardService::new(database.clone(), candidates.clone());
        let connection = database.open().unwrap();
        let key = calculate_key(&normalize_identifier(workspace.path().to_str().unwrap()).unwrap());
        connection
            .execute(
                "INSERT INTO project(id,name,description,color,is_archived,workspace_key,workspace_label,created_at,updated_at) \
                 VALUES('22222222-2222-4222-8222-222222222222','卡片项目','','#238f7a',0,$key,$label,'2026-08-21T08:00:00+00:00','2026-08-21T08:00:00+00:00');",
                params![key, workspace.path().to_str().unwrap()],
            )
            .unwrap();
        drop(connection);
        TestContext {
            cards,
            candidates,
            database,
            workspace,
            _directory: temp,
            caller: McpCallerContext {
                token_id: "00000000-0000-4000-8000-000000000001".to_string(),
                display_name: "Codex".to_string(),
                permission: McpPermission::ReadWrite,
                project_id: None,
            },
        }
    }

    fn workspace_path(context: &TestContext) -> String {
        context.workspace.path().to_str().unwrap().to_string()
    }

    fn payload() -> ConclusionCardPayload {
        ConclusionCardPayload {
            title: "启动崩溃 → 配置解析顺序错误".to_string(),
            problem_id: Some("PROB-20260821-001".to_string()),
            problem_description: "服务启动即崩溃，日志指向配置解析".to_string(),
            final_conclusion: "配置解析必须在日志初始化之后执行".to_string(),
            root_cause: "解析器读取了尚未初始化的日志上下文".to_string(),
            applicable_conditions: vec!["同类启动流程".to_string()],
            not_applicable_conditions: vec![],
            evidence: vec!["崩溃堆栈指向 ConfigParser".to_string()],
            verified_results: vec!["修复后连续启动 10 次成功".to_string()],
            failed_attempts: vec!["延迟整体启动：未解决，仅推迟了崩溃".to_string()],
            do_not_repeat: vec!["不要在日志初始化前解析配置".to_string()],
            retry_conditions: vec!["若重构启动顺序可重新评估".to_string()],
            next_steps: vec!["补充启动顺序回归测试".to_string()],
            keywords: vec!["启动".to_string(), "配置".to_string()],
            tags: vec!["后端".to_string()],
            importance: 4,
            importance_reason: "根因清晰且失败路径容易被重复".to_string(),
            cloud_embedding_allowed: false,
            resolved_at: "2026-08-21T08:00:00Z".to_string(),
        }
    }

    #[test]
    fn submit_creates_solution_candidate_with_payload() {
        let context = context();
        let candidate = context
            .cards
            .submit_candidate(&workspace_path(&context), &payload(), "Codex")
            .unwrap();
        assert_eq!(candidate.memory_type, "SOLUTION");
        assert_eq!(candidate.scope, MemoryScope::Project);
        assert_eq!(
            candidate.project_id.as_deref(),
            Some("22222222-2222-4222-8222-222222222222")
        );
        assert!(!candidate.cloud_processing_allowed, "卡片自身禁止云端嵌入");
        assert!(candidate.content.contains("## 根本原因"));
        // 正式记忆 / FTS / 任务表未被触碰。
        let connection = context.database.open().unwrap();
        for table in ["memory", "memory_fts", "background_task", "conclusion_card"] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table};"), [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table} 不应被候选写入触碰");
        }
        // 载荷可读回。
        let stored = context.cards.get_candidate_payload(&candidate.id).unwrap().unwrap();
        assert_eq!(stored.problem_id.as_deref(), Some("PROB-20260821-001"));
    }

    #[test]
    fn missing_required_fields_are_rejected() {
        let context = context();
        let mut broken = payload();
        broken.evidence = vec![];
        let error = context
            .cards
            .submit_candidate(&workspace_path(&context), &broken, "Codex")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ConclusionCardFieldRequired);
        assert!(error.message.contains("证据"));

        let mut no_conclusion = payload();
        no_conclusion.final_conclusion = "  ".to_string();
        let error = context
            .cards
            .submit_candidate(&workspace_path(&context), &no_conclusion, "Codex")
            .unwrap_err();
        assert!(error.message.contains("最终结论"));

        let mut bad_importance = payload();
        bad_importance.importance = 8;
        let error = context
            .cards
            .submit_candidate(&workspace_path(&context), &bad_importance, "Codex")
            .unwrap_err();
        assert!(error.message.contains("重要度"));
    }

    #[test]
    fn problem_id_allows_explicit_null() {
        let context = context();
        let mut card = payload();
        card.problem_id = None;
        let candidate = context
            .cards
            .submit_candidate(&workspace_path(&context), &card, "Codex")
            .unwrap();
        let stored = context.cards.get_candidate_payload(&candidate.id).unwrap().unwrap();
        assert!(stored.problem_id.is_none());
    }

    #[test]
    fn confirm_creates_solution_memory_and_extension() {
        let context = context();
        let candidate = context
            .cards
            .submit_candidate(&workspace_path(&context), &payload(), "Codex")
            .unwrap();
        let memory = context
            .candidates
            .confirm(&candidate.id, candidate.version, &context.caller)
            .unwrap();
        assert_eq!(memory.memory_type, "SOLUTION");
        // FTS 已同步；conclusion_card 扩展已创建。
        let connection = context.database.open().unwrap();
        let fts: i64 = connection
            .query_row("SELECT count(*) FROM memory_fts;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fts, 1);
        let (problem_id, importance): (Option<String>, i64) = connection
            .query_row(
                "SELECT problem_id,json_extract(structured_payload_json,'$.importance') \
                 FROM conclusion_card WHERE memory_id=$id;",
                params![memory.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(problem_id.as_deref(), Some("PROB-20260821-001"));
        assert_eq!(importance, 4);
        // 云端嵌入禁止 → 无 Embedding 任务（图谱任务仍会排队）。
        let embedding_tasks: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='EMBED_MEMORY';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(embedding_tasks, 0);
        // 候选已删除。
        let error = context.candidates.get(&candidate.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateNotFound);
    }

    #[test]
    fn confirm_with_embedding_allowed_queues_task() {
        let context = context();
        let mut card = payload();
        card.cloud_embedding_allowed = true;
        let candidate = context
            .cards
            .submit_candidate(&workspace_path(&context), &card, "Codex")
            .unwrap();
        assert!(candidate.cloud_processing_allowed);
        context
            .candidates
            .confirm(&candidate.id, candidate.version, &context.caller)
            .unwrap();
        let connection = context.database.open().unwrap();
        let embedding_tasks: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='EMBED_MEMORY';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(embedding_tasks, 1, "卡片允许云端嵌入时应排队 Embedding 任务");
    }

    #[test]
    fn update_payload_regenerates_content_with_optimistic_lock() {
        let context = context();
        let candidate = context
            .cards
            .submit_candidate(&workspace_path(&context), &payload(), "Codex")
            .unwrap();
        let mut edited = payload();
        edited.final_conclusion = "修订后的结论".to_string();
        edited.importance = 5;
        let updated = context
            .cards
            .update_candidate_payload(&candidate.id, &edited, candidate.version)
            .unwrap();
        assert_eq!(updated.version, 2);
        assert!(updated.content.contains("修订后的结论"));
        let stored = context.cards.get_candidate_payload(&candidate.id).unwrap().unwrap();
        assert_eq!(stored.importance, 5);
        // 过期版本冲突。
        let error = context
            .cards
            .update_candidate_payload(&candidate.id, &edited, 1)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::MemoryCandidateVersionConflict);
    }

    #[test]
    fn reject_deletes_candidate_and_payload() {
        let context = context();
        let candidate = context
            .cards
            .submit_candidate(&workspace_path(&context), &payload(), "Codex")
            .unwrap();
        context
            .candidates
            .reject(&candidate.id, candidate.version, &context.caller)
            .unwrap();
        assert!(context.cards.get_candidate_payload(&candidate.id).unwrap().is_none());
        let connection = context.database.open().unwrap();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM conclusion_card;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "拒绝候选不得产生正式卡片");
    }

    #[test]
    fn unbound_workspace_rejects_submit() {
        let context = context();
        let elsewhere = tempfile::tempdir().unwrap();
        let error = context
            .cards
            .submit_candidate(elsewhere.path().to_str().unwrap(), &payload(), "Codex")
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentWorkspaceUnbound);
    }

    /// 端到端：确认卡片后，项目交接能按问题编号召回相关卡片。
    #[test]
    fn handoff_recalls_related_conclusion_cards() {
        let context = context();
        let candidate = context
            .cards
            .submit_candidate(&workspace_path(&context), &payload(), "Codex")
            .unwrap();
        let memory = context
            .candidates
            .confirm(&candidate.id, candidate.version, &context.caller)
            .unwrap();

        // 建立项目文档现场（直接用 ProjectDocumentService）。
        let clock = Arc::new(FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 21, 8, 0, 0).unwrap()));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=40)
                .map(|index| format!("{index:08}-{index:04}-4{index:03}-8{index:03}-{index:012}"))
                .collect(),
        ));
        let documents = Arc::new(crate::project_document_service::ProjectDocumentService::new(
            context.database.clone(),
            clock,
            ids,
        ));
        let docs: Vec<(memory_domain::ProjectDocumentType, String)> = memory_domain::ALL_PROJECT_DOCUMENT_TYPES
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    format!(
                        "# {}\n\n## 当前问题\n\n### PROB-20260821-001 启动失败\n\n严重程度：高。\n\n## 已解决问题\n\n（无）",
                        kind.display_name()
                    ),
                )
            })
            .collect();
        documents.create_drafts(&workspace_path(&context), &docs, None).unwrap();
        for kind in memory_domain::ALL_PROJECT_DOCUMENT_TYPES {
            documents
                .approve_draft("22222222-2222-4222-8222-222222222222", kind)
                .unwrap();
        }
        documents
            .promote_drafts("22222222-2222-4222-8222-222222222222")
            .unwrap();

        let request = ProjectHandoffRequest {
            workspace_path: workspace_path(&context),
            context_max_chars: 1000,
            active_decision_max_count: 5,
            current_status_max_chars: 1000,
            active_problem_max_count: 5,
            resolved_problem_max_count: 3,
            recent_changelog_max_count: 5,
            related_conclusion_card_max_count: 3,
            conclusion_card_max_chars: 200,
        };
        let result = documents.handoff(&request).unwrap();
        assert_eq!(result.status, "ACTIVE");
        assert_eq!(result.related_conclusion_cards.len(), 1);
        assert_eq!(result.related_conclusion_cards[0].memory_id, memory.id);
        assert!(result.related_conclusion_cards[0].content.contains("配置解析"));
    }
}
