//! 项目文档应用服务：初始化草稿、审核晋升、正式文档同步与项目交接
//! （执行计划 §9–§16）。
//!
//! 核心语义：
//! - 正式 Markdown 文件是唯一事实来源；数据库 `project_document` 只是镜像。
//! - 每次读取（交接 / 总览 / 批量更新前）都执行完整校验和比对与恢复（§13.4），
//!   监听器只是及时性优化，不作为一致性保障。
//! - 所有 AI 写入走乐观锁；任意文档冲突时整批拒绝（§14.2）。
//! - 五份草稿全部批准才晋升；晋升过程可恢复（§12）。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;

use memory_domain::{
    ALL_PROJECT_DOCUMENT_TYPES, BusinessError, ConclusionCardBrief, ErrorCode, ProjectDocumentDraftItem,
    ProjectDocumentItem, ProjectDocumentState, ProjectDocumentType, ProjectDocumentUpdateResultItem,
    ProjectHandoffRequest, ProjectHandoffResult,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::project_document_fs::{
    WorkspacePaths, atomic_write, cleanup_temp_files, document_checksum, extract_body_lossy, parse_document,
    read_text_if_exists, remove_if_exists, render_document,
};
use crate::project_document_watcher::{self, WatchLease};
use crate::sqlite_errors::map_sqlite_error;
use crate::tokenizer::tokenize;
use crate::workspace_identity::{calculate_key, normalize_identifier};

/// 项目文档状态机文本（§8.1）。
pub mod status {
    pub const NOT_INITIALIZED: &str = "NOT_INITIALIZED";
    pub const DRAFT_PENDING_REVIEW: &str = "DRAFT_PENDING_REVIEW";
    pub const DRAFT_PARTIALLY_APPROVED: &str = "DRAFT_PARTIALLY_APPROVED";
    pub const DRAFT_ALL_APPROVED: &str = "DRAFT_ALL_APPROVED";
    pub const PROMOTING: &str = "PROMOTING";
    pub const ACTIVE: &str = "ACTIVE";
}

/// 绑定项目行（工作空间解析结果）。
#[derive(Debug, Clone)]
struct ProjectBinding {
    id: String,
    name: String,
}

/// 一次同步后的工作区结论（供总览与交接复用）。
struct SyncOutcome {
    status: &'static str,
    warnings: Vec<String>,
    states: Vec<ProjectDocumentState>,
    mirrors: BTreeMap<ProjectDocumentType, DocumentMirror>,
}

/// 正式文档镜像行。
#[derive(Debug, Clone)]
struct DocumentMirror {
    content: String,
    checksum: String,
    version: i64,
}

/// 一次批量更新中单份文档的恢复信息。
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchUpdateJournalEntry {
    document_type: ProjectDocumentType,
    previous_content: String,
    next_content: String,
    next_checksum: String,
    next_version: i64,
}

/// 正式文档批量更新的持久化恢复日志。
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchUpdateJournal {
    project_id: String,
    entries: Vec<BatchUpdateJournalEntry>,
}

/// 管理项目全局文档的创建、审核、晋升、同步与交接。
pub struct ProjectDocumentService {
    database: Database,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    watcher: Mutex<Option<WatchLease>>,
}

impl ProjectDocumentService {
    /// 创建项目文档应用服务。
    pub fn new(database: Database, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self {
            database,
            clock,
            ids,
            watcher: Mutex::new(None),
        }
    }

    // -----------------------------------------------------------------------
    // 工作空间 → 项目绑定
    // -----------------------------------------------------------------------

    /// 按工作空间绝对路径解析绑定项目（复用工作空间标识规范化规则）。
    fn resolve_binding(&self, workspace_path: &str) -> Result<(ProjectBinding, WorkspacePaths), BusinessError> {
        let paths = WorkspacePaths::resolve(workspace_path)?;
        let identifier = normalize_identifier(workspace_path)?;
        let key = calculate_key(&identifier);
        let connection = self.database.open()?;
        let row = connection
            .query_row(
                "SELECT id,name FROM project WHERE workspace_key=$key;",
                params![key],
                |row| {
                    Ok(ProjectBinding {
                        id: row.get(0)?,
                        name: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(map_sqlite_error)?;
        let binding = row.ok_or_else(|| {
            BusinessError::with_message(
                ErrorCode::ProjectDocumentWorkspaceUnbound,
                format!("工作空间「{identifier}」尚未绑定 MemStack 项目，项目文档能力未启用"),
            )
        })?;
        Ok((binding, paths))
    }

    /// 按工作空间绝对路径解析绑定项目 ID（未绑定返回 `None`；供 MCP 权限检查）。
    pub fn resolve_project_id(&self, workspace_path: &str) -> Result<Option<String>, BusinessError> {
        WorkspacePaths::resolve(workspace_path)?;
        let identifier = normalize_identifier(workspace_path)?;
        let key = calculate_key(&identifier);
        let connection = self.database.open()?;
        connection
            .query_row(
                "SELECT id FROM project WHERE workspace_key=$key;",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(map_sqlite_error)
    }

    /// 记录最近一次项目文档操作的工作空间绝对路径（供桌面端定位文件）。
    fn remember_workspace_path(&self, project_id: &str, paths: &WorkspacePaths) -> Result<(), BusinessError> {
        let display = paths
            .root()
            .to_str()
            .map(|text| text.trim_start_matches(r"\\?\").to_string())
            .unwrap_or_default();
        let connection = self.database.open()?;
        connection
            .execute(
                "UPDATE project SET project_document_workspace_path=$path WHERE id=$id;",
                params![display, project_id],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 读取项目记录的工作空间路径与 Embedding 开关。
    pub fn get_project_settings(&self, project_id: &str) -> Result<(Option<String>, bool), BusinessError> {
        let connection = self.database.open()?;
        connection
            .query_row(
                "SELECT project_document_workspace_path,project_document_embedding_enabled \
                 FROM project WHERE id=$id;",
                params![project_id],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(map_sqlite_error)?
            .ok_or_else(|| BusinessError::new(ErrorCode::ProjectNotFound))
    }

    /// 更新项目文档 Embedding 开关（显式传值，默认关闭由调用方保证）。
    pub fn set_embedding_enabled(&self, project_id: &str, enabled: bool) -> Result<bool, BusinessError> {
        let connection = self.database.open()?;
        let changed = connection
            .execute(
                "UPDATE project SET project_document_embedding_enabled=$enabled WHERE id=$id;",
                params![enabled as i64, project_id],
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            return Err(BusinessError::new(ErrorCode::ProjectNotFound));
        }
        // 同步五份文档镜像行的开关标记（向量索引随现有任务机制生效）。
        connection
            .execute(
                "UPDATE project_document SET embedding_enabled=$enabled WHERE project_id=$id;",
                params![enabled as i64, project_id],
            )
            .map_err(map_sqlite_error)?;
        if enabled {
            queue_project_document_embeddings(&connection, project_id, &format_storage_time(self.clock.now_utc()))?;
        } else {
            connection
                .execute(
                    "DELETE FROM project_document_embedding WHERE project_id=$id;",
                    params![project_id],
                )
                .map_err(map_sqlite_error)?;
            connection
                .execute(
                    "DELETE FROM background_task WHERE task_type='EMBED_PROJECT_DOCUMENT' AND target_id IN \
                       (SELECT id FROM project_document WHERE project_id=$id);",
                    params![project_id],
                )
                .map_err(map_sqlite_error)?;
        }
        Ok(enabled)
    }

    // -----------------------------------------------------------------------
    // 草稿：创建 / 读取 / 更新 / 审核 / 删除（阶段 3）
    // -----------------------------------------------------------------------

    /// 项目未初始化时一次创建五份真实草稿（§9.3）。
    ///
    /// - 已存在草稿（行或磁盘文件）→ 导入 / 返回现有草稿，不重复创建。
    /// - 已存在正式文档（行或文件）→ `PROJECT_DOCUMENT_ALREADY_ACTIVE` 拒绝。
    pub fn create_drafts(
        &self,
        workspace_path: &str,
        documents: &[(ProjectDocumentType, String)],
        change_reason: Option<&str>,
    ) -> Result<Vec<ProjectDocumentDraftItem>, BusinessError> {
        let (binding, paths) = self.resolve_binding(workspace_path)?;
        validate_document_set(documents)?;
        let mut connection = self.database.open()?;
        if count_document_rows(&connection, &binding.id)? > 0 || formal_files_present(&paths) {
            return Err(BusinessError::new(ErrorCode::ProjectDocumentAlreadyActive));
        }
        // 幂等：已有草稿行 → 返回现有（先尝试磁盘导入补齐缺失行）。
        let existing = self.load_drafts(&connection, &binding.id)?;
        if existing.len() == ALL_PROJECT_DOCUMENT_TYPES.len() {
            return Ok(existing);
        }
        if !existing.is_empty() {
            return Err(BusinessError::new(ErrorCode::ProjectDocumentDraftIncomplete));
        }
        // 磁盘残留草稿（上次创建写文件后、写库前中断）：以磁盘为准导入。
        let disk_drafts = read_all_draft_files(&paths)?;
        let source: Vec<(ProjectDocumentType, String)> = if disk_drafts.len() == ALL_PROJECT_DOCUMENT_TYPES.len() {
            disk_drafts
        } else {
            documents.to_vec()
        };
        let now_text = format_storage_time(self.clock.now_utc());
        paths.ensure_memstack_dirs()?;
        paths.ensure_git_exclude()?;
        let rendered: Vec<(ProjectDocumentType, String)> = source
            .iter()
            .map(|(kind, body)| {
                let content = render_document(*kind, body);
                parse_document(*kind, &content)?;
                Ok((*kind, content))
            })
            .collect::<Result<_, BusinessError>>()?;
        for (kind, content) in &rendered {
            atomic_write(&paths.draft_path(*kind), content)?;
        }
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        for (kind, content) in &rendered {
            insert_draft_row(
                &transaction,
                &self.ids.new_id(),
                &binding.id,
                *kind,
                content,
                change_reason,
                &now_text,
            )?;
        }
        transaction.commit().map_err(map_sqlite_error)?;
        self.remember_workspace_path(&binding.id, &paths)?;
        drop(connection);
        self.list_drafts_by_project(&binding.id)
    }

    /// 列出项目的全部草稿（按固定类型顺序）。
    pub fn list_drafts_by_project(&self, project_id: &str) -> Result<Vec<ProjectDocumentDraftItem>, BusinessError> {
        let connection = self.database.open()?;
        self.load_drafts(&connection, project_id)
    }

    fn load_drafts(
        &self,
        connection: &Connection,
        project_id: &str,
    ) -> Result<Vec<ProjectDocumentDraftItem>, BusinessError> {
        let mut statement = connection
            .prepare(
                "SELECT id,project_id,document_type,relative_path,content,checksum,version, \
                        review_status,approved_version,last_change_reason,created_at,updated_at \
                 FROM project_document_draft WHERE project_id=$pid ORDER BY document_type;",
            )
            .map_err(map_sqlite_error)?;
        let rows = statement
            .query_map(params![project_id], read_draft_row)
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(rows)
    }

    /// 读取单份草稿。
    pub fn get_draft(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
    ) -> Result<ProjectDocumentDraftItem, BusinessError> {
        let connection = self.database.open()?;
        let mut statement = connection
            .prepare(
                "SELECT id,project_id,document_type,relative_path,content,checksum,version, \
                        review_status,approved_version,last_change_reason,created_at,updated_at \
                 FROM project_document_draft WHERE project_id=$pid AND document_type=$type;",
            )
            .map_err(map_sqlite_error)?;
        statement
            .query_row(params![project_id, document_type.as_str()], read_draft_row)
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => BusinessError::new(ErrorCode::ProjectDocumentNotFound),
                other => map_sqlite_error(other),
            })
    }

    /// 更新一份草稿：乐观锁 + 版本递增 + 审核状态回退为待审核（§9.4）。
    pub fn update_draft(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
        expected_version: i64,
        content: &str,
        change_reason: &str,
    ) -> Result<ProjectDocumentDraftItem, BusinessError> {
        if change_reason.trim().is_empty() {
            return Err(BusinessError::with_message(
                ErrorCode::InvalidArgument,
                "更新草稿必须说明变化原因（change_reason）",
            ));
        }
        let (workspace_path, _) = self.get_project_settings(project_id)?;
        let paths = self.require_workspace_paths(project_id, workspace_path.as_deref())?;
        let connection = self.database.open()?;
        let current = self.get_draft(project_id, document_type)?;
        if current.version != expected_version {
            return Err(version_conflict(
                document_type,
                &current.content,
                current.version,
                &current.checksum,
            ));
        }
        let rendered = render_document(document_type, content);
        parse_document(document_type, &rendered)?;
        atomic_write(&paths.draft_path(document_type), &rendered)?;
        let now_text = format_storage_time(self.clock.now_utc());
        let changed = connection
            .execute(
                "UPDATE project_document_draft \
                 SET content=$content,checksum=$checksum,version=version+1, \
                     review_status='PENDING_REVIEW',last_change_reason=$reason,updated_at=$updated \
                 WHERE id=$id AND version=$expected;",
                params![
                    rendered,
                    document_checksum(&rendered),
                    change_reason.trim(),
                    now_text,
                    current.id,
                    expected_version
                ],
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            return Err(version_conflict(
                document_type,
                &current.content,
                current.version,
                &current.checksum,
            ));
        }
        self.get_draft(project_id, document_type)
    }

    /// 批准单份草稿（记录批准版本）。
    pub fn approve_draft(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
    ) -> Result<ProjectDocumentDraftItem, BusinessError> {
        self.set_draft_review(project_id, document_type, true, None)
    }

    /// 撤销单份草稿批准。
    pub fn revoke_draft_approval(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
    ) -> Result<ProjectDocumentDraftItem, BusinessError> {
        self.set_draft_review(project_id, document_type, false, None)
    }

    fn set_draft_review(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
        approve: bool,
        expected_version: Option<i64>,
    ) -> Result<ProjectDocumentDraftItem, BusinessError> {
        let connection = self.database.open()?;
        let current = self.get_draft(project_id, document_type)?;
        if let Some(expected) = expected_version
            && expected != current.version
        {
            return Err(version_conflict(
                document_type,
                &current.content,
                current.version,
                &current.checksum,
            ));
        }
        let now_text = format_storage_time(self.clock.now_utc());
        connection
            .execute(
                "UPDATE project_document_draft \
                 SET review_status=$status,approved_version=$approved,updated_at=$updated \
                 WHERE id=$id;",
                params![
                    if approve { "APPROVED" } else { "PENDING_REVIEW" },
                    if approve {
                        Some(current.version)
                    } else {
                        current.approved_version
                    },
                    now_text,
                    current.id
                ],
            )
            .map_err(map_sqlite_error)?;
        self.get_draft(project_id, document_type)
    }

    /// 删除全部初始化草稿（行 + 磁盘目录）。
    pub fn delete_drafts(&self, project_id: &str) -> Result<(), BusinessError> {
        let (workspace_path, _) = self.get_project_settings(project_id)?;
        let paths = self.require_workspace_paths(project_id, workspace_path.as_deref())?;
        let connection = self.database.open()?;
        connection
            .execute(
                "DELETE FROM project_document_draft WHERE project_id=$pid;",
                params![project_id],
            )
            .map_err(map_sqlite_error)?;
        remove_if_exists(&paths.drafts_root())?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 晋升与恢复（§12）
    // -----------------------------------------------------------------------

    /// 五份草稿全部批准后晋升为正式文档（§12.1）。
    pub fn promote_drafts(&self, project_id: &str) -> Result<Vec<ProjectDocumentItem>, BusinessError> {
        let (workspace_path, _) = self.get_project_settings(project_id)?;
        let paths = self.require_workspace_paths(project_id, workspace_path.as_deref())?;
        self.with_workspace_sync_lock(&paths, || self.promote_drafts_locked(project_id, &paths))
    }

    /// 在工作空间同步锁内晋升五份已批准草稿。
    fn promote_drafts_locked(
        &self,
        project_id: &str,
        paths: &WorkspacePaths,
    ) -> Result<Vec<ProjectDocumentItem>, BusinessError> {
        let mut connection = self.database.open()?;
        let drafts = self.load_drafts(&connection, project_id)?;
        if drafts.len() != ALL_PROJECT_DOCUMENT_TYPES.len() {
            return Err(BusinessError::new(ErrorCode::ProjectDocumentDraftIncomplete));
        }
        for draft in &drafts {
            if draft.review_status != "APPROVED" || draft.approved_version != Some(draft.version) {
                return Err(BusinessError::with_message(
                    ErrorCode::ProjectDocumentDraftIncomplete,
                    format!(
                        "草稿「{}」尚未批准（或批准后又被修改），不能晋升",
                        draft.document_type.display_name()
                    ),
                ));
            }
        }
        // 再次校验内容（文件名、YAML、类型、正文）。
        for draft in &drafts {
            parse_document(draft.document_type, &draft.content)?;
        }
        self.execute_promotion(project_id, paths, &mut connection, &drafts)?;
        self.remember_workspace_path(project_id, paths)?;
        self.list_documents(project_id)
    }

    /// 晋升执行体：建立记录 → 同盘暂存 → 原子替换 → 镜像 → 清理。
    fn execute_promotion(
        &self,
        project_id: &str,
        paths: &WorkspacePaths,
        connection: &mut Connection,
        drafts: &[ProjectDocumentDraftItem],
    ) -> Result<(), BusinessError> {
        let now_text = format_storage_time(self.clock.now_utc());
        let promotion_id = self.ids.new_id();
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO project_document_promotion(id,project_id,status,error_message,created_at,updated_at) \
                 VALUES($id,$pid,'IN_PROGRESS',NULL,$now,$now);",
                params![promotion_id, project_id, now_text],
            )
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)?;

        let result = self.promotion_write_phase(project_id, paths, connection, drafts, &now_text);

        let finish = connection.transaction().map_err(map_sqlite_error)?;
        match result {
            Ok(()) => {
                finish
                    .execute(
                        "UPDATE project_document_promotion SET status='COMPLETED',updated_at=$now WHERE id=$id;",
                        params![now_text, promotion_id],
                    )
                    .map_err(map_sqlite_error)?;
                // 晋升成功：删除初始化草稿（行 + 目录）。
                finish
                    .execute(
                        "DELETE FROM project_document_draft WHERE project_id=$pid;",
                        params![project_id],
                    )
                    .map_err(map_sqlite_error)?;
                finish.commit().map_err(map_sqlite_error)?;
                remove_if_exists(&paths.drafts_root())?;
                cleanup_temp_files(&paths.memstack_dir());
                Ok(())
            }
            Err(error) => {
                finish
                    .execute(
                        "UPDATE project_document_promotion SET status='FAILED',error_message=$message,updated_at=$now WHERE id=$id;",
                        params![error.message, now_text, promotion_id],
                    )
                    .map_err(map_sqlite_error)?;
                finish.commit().map_err(map_sqlite_error)?;
                cleanup_temp_files(&paths.memstack_dir());
                Err(error)
            }
        }
    }

    /// 晋升写入阶段：目录准备 → 同盘暂存五份文件 → 原子替换 → 更新镜像。
    fn promotion_write_phase(
        &self,
        project_id: &str,
        paths: &WorkspacePaths,
        connection: &mut Connection,
        drafts: &[ProjectDocumentDraftItem],
        now_text: &str,
    ) -> Result<(), BusinessError> {
        paths.ensure_memstack_dirs()?;
        paths.ensure_git_exclude()?;
        // 同盘暂存（临时文件位于 .memstack 内，与目标同卷）→ 逐份原子替换。
        for draft in drafts {
            atomic_write(&paths.document_path(draft.document_type), &draft.content)?;
        }
        // 更新五份镜像（已有行则续版本，首次为 1）。
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        for draft in drafts {
            upsert_document_mirror(
                &transaction,
                &self.ids.new_id(),
                project_id,
                draft.document_type,
                &draft.content,
                now_text,
            )?;
        }
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // 同步与状态（阶段 4，§12.2 / §13）
    // -----------------------------------------------------------------------

    /// 对工作区执行完整同步，并用命名互斥锁串行化跨进程同步。
    fn sync_workspace(&self, project_id: &str, paths: &WorkspacePaths) -> Result<SyncOutcome, BusinessError> {
        self.with_workspace_sync_lock(paths, || self.sync_workspace_unlocked(project_id, paths))
    }

    /// 在工作空间级跨进程互斥锁内执行一个完整文件与数据库操作。
    fn with_workspace_sync_lock<T>(
        &self,
        paths: &WorkspacePaths,
        operation: impl FnOnce() -> Result<T, BusinessError>,
    ) -> Result<T, BusinessError> {
        let mutex = memory_platform::NamedMutex::create(&project_document_watcher::sync_mutex_name(paths))?;
        match mutex.wait(10_000)? {
            memory_platform::WaitResult::Acquired | memory_platform::WaitResult::Abandoned => {}
            memory_platform::WaitResult::Timeout => {
                return Err(BusinessError::with_message(
                    ErrorCode::DatabaseBusy,
                    "等待项目文档跨进程同步超时，请稍后重试",
                ));
            }
        }
        let result = operation();
        let release_result = mutex.release();
        match (result, release_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    /// 执行单次完整同步：临时文件清理、晋升恢复、文件 ↔ 镜像校验和比对。
    fn sync_workspace_unlocked(&self, project_id: &str, paths: &WorkspacePaths) -> Result<SyncOutcome, BusinessError> {
        self.recover_batch_update(project_id, paths)?;
        cleanup_temp_files(&paths.memstack_dir());
        let mut warnings: Vec<String> = Vec::new();
        let mut connection = self.database.open()?;
        let mut mirrors = load_document_mirrors(&connection, project_id)?;

        // 晋升恢复（§12.2）：只有部分正式文件时，用已批准草稿补齐。
        let file_states = read_all_document_files(paths)?;
        let present = file_states.iter().filter(|(_, state)| state.is_some()).count();
        if present > 0 && present < ALL_PROJECT_DOCUMENT_TYPES.len() {
            let drafts = self.load_drafts(&connection, project_id)?;
            let all_approved = drafts.len() == ALL_PROJECT_DOCUMENT_TYPES.len()
                && drafts
                    .iter()
                    .all(|draft| draft.review_status == "APPROVED" && draft.approved_version == Some(draft.version));
            if all_approved {
                self.execute_promotion(project_id, paths, &mut connection, &drafts)?;
                warnings.push("检测到中断的晋升，已用已批准草稿自动恢复完成".to_string());
                mirrors = load_document_mirrors(&connection, project_id)?;
            }
        }

        let mut states: Vec<ProjectDocumentState> = Vec::new();
        let now_text = format_storage_time(self.clock.now_utc());
        for kind in ALL_PROJECT_DOCUMENT_TYPES {
            let path = paths.document_path(kind);
            let file = read_text_if_exists(&path)?;
            let file = match file {
                Some(content) => Some(content),
                None => {
                    // 文件缺失：从镜像恢复（§12.2）。
                    if let Some(mirror) = mirrors.get(&kind) {
                        atomic_write(&path, &mirror.content)?;
                        warnings.push(format!("文件缺失，已从数据库镜像恢复：{}", kind.file_name()));
                        Some(mirror.content.clone())
                    } else {
                        None
                    }
                }
            };
            let Some(content) = file else {
                continue;
            };
            let checksum = document_checksum(&content);
            let parse_result = parse_document(kind, &content);
            let transaction = connection.transaction().map_err(map_sqlite_error)?;
            let (version, sync_status) = match parse_result {
                Ok(_) => match mirrors.get(&kind) {
                    None => {
                        // 本地新增正式文件（如手动拷贝）：导入镜像。
                        upsert_document_mirror(
                            &transaction,
                            &self.ids.new_id(),
                            project_id,
                            kind,
                            &content,
                            &now_text,
                        )?;
                        warnings.push(format!("已导入本地新增的正式文档：{}", kind.file_name()));
                        (1, "SYNCED")
                    }
                    Some(mirror) if mirror.checksum == checksum => (mirror.version, "SYNCED"),
                    Some(mirror) => {
                        // 文件为事实来源：同步外部修改并保留上一版快照（§13.3）。
                        update_mirror_from_file(&transaction, project_id, kind, &content, &now_text)?;
                        warnings.push(format!("已同步外部修改：{}", kind.file_name()));
                        (mirror.version + 1, "SYNCED")
                    }
                },
                Err(error) => {
                    // YAML 错误：不覆盖用户文件（§15.1），只标记格式错误。
                    if mirrors.contains_key(&kind) {
                        set_document_sync_status(&transaction, project_id, kind, "FORMAT_ERROR")?;
                    }
                    warnings.push(format!("{} 格式错误：{}", kind.file_name(), error.message));
                    let version = mirrors.get(&kind).map(|mirror| mirror.version).unwrap_or(0);
                    drop(transaction);
                    states.push(ProjectDocumentState {
                        document_type: kind,
                        relative_path: kind.file_name().to_string(),
                        version,
                        checksum_prefix: checksum.chars().take(12).collect(),
                        sync_status: "FORMAT_ERROR".to_string(),
                        updated_at: now_text.clone(),
                        file_exists: true,
                    });
                    continue;
                }
            };
            transaction.commit().map_err(map_sqlite_error)?;
            self.refresh_document_indexes(project_id, kind, &content)?;
            states.push(ProjectDocumentState {
                document_type: kind,
                relative_path: kind.file_name().to_string(),
                version,
                checksum_prefix: checksum.chars().take(12).collect(),
                sync_status: sync_status.to_string(),
                updated_at: now_text.clone(),
                file_exists: true,
            });
        }
        mirrors = load_document_mirrors(&connection, project_id)?;

        // 状态判定（§8.2）。
        let valid_files = states
            .iter()
            .filter(|state| state.sync_status != "FORMAT_ERROR")
            .count();
        let status: &'static str = if states.len() == ALL_PROJECT_DOCUMENT_TYPES.len()
            && valid_files == ALL_PROJECT_DOCUMENT_TYPES.len()
            && mirrors.len() == ALL_PROJECT_DOCUMENT_TYPES.len()
        {
            // ACTIVE：正式文档优先，但保留残留草稿交由用户处理。
            let drafts = self.load_drafts(&connection, project_id)?;
            if !drafts.is_empty() {
                warnings.push("正式文档已生效，但仍存在初始化草稿；草稿已保留，请确认后手动处理".to_string());
            }
            status::ACTIVE
        } else if !states.is_empty() {
            status::PROMOTING
        } else {
            let drafts = self.load_drafts(&connection, project_id)?;
            if drafts.len() == ALL_PROJECT_DOCUMENT_TYPES.len() {
                let approved = drafts
                    .iter()
                    .filter(|draft| draft.review_status == "APPROVED" && draft.approved_version == Some(draft.version))
                    .count();
                match approved {
                    0 => status::DRAFT_PENDING_REVIEW,
                    count if count == ALL_PROJECT_DOCUMENT_TYPES.len() => status::DRAFT_ALL_APPROVED,
                    _ => status::DRAFT_PARTIALLY_APPROVED,
                }
            } else if !drafts.is_empty() {
                status::DRAFT_PENDING_REVIEW
            } else {
                status::NOT_INITIALIZED
            }
        };
        Ok(SyncOutcome {
            status,
            warnings,
            states,
            mirrors,
        })
    }

    fn require_workspace_paths(
        &self,
        _project_id: &str,
        workspace_path: Option<&str>,
    ) -> Result<WorkspacePaths, BusinessError> {
        let Some(path) = workspace_path.filter(|value| !value.trim().is_empty()) else {
            return Err(BusinessError::with_message(
                ErrorCode::ProjectDocumentWorkspaceUnbound,
                "该项目尚未记录工作空间路径：请先通过 MCP 在该工作空间调用项目文档工具",
            ));
        };
        WorkspacePaths::resolve(path)
    }

    // -----------------------------------------------------------------------
    // 正式文档读取与批量更新（阶段 4 / 5）
    // -----------------------------------------------------------------------

    /// 列出项目全部正式文档（含同步状态；会先执行同步）。
    pub fn list_documents(&self, project_id: &str) -> Result<Vec<ProjectDocumentItem>, BusinessError> {
        let (workspace_path, _) = self.get_project_settings(project_id)?;
        let paths = self.require_workspace_paths(project_id, workspace_path.as_deref())?;
        self.sync_workspace(project_id, &paths)?;
        let connection = self.database.open()?;
        let mut statement = connection
            .prepare(
                "SELECT id,project_id,document_type,relative_path,content,checksum,version, \
                        previous_version,embedding_enabled,sync_status,created_at,updated_at \
                 FROM project_document WHERE project_id=$pid ORDER BY document_type;",
            )
            .map_err(map_sqlite_error)?;
        let rows = statement
            .query_map(params![project_id], read_document_row)
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(rows)
    }

    /// 读取单份正式文档。
    pub fn get_document(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
    ) -> Result<ProjectDocumentItem, BusinessError> {
        let connection = self.database.open()?;
        connection
            .query_row(
                "SELECT id,project_id,document_type,relative_path,content,checksum,version, \
                        previous_version,embedding_enabled,sync_status,created_at,updated_at \
                 FROM project_document WHERE project_id=$pid AND document_type=$type;",
                params![project_id, document_type.as_str()],
                read_document_row,
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => BusinessError::new(ErrorCode::ProjectDocumentNotFound),
                other => map_sqlite_error(other),
            })
    }

    /// 批量更新正式文档：先全部校验（版本 + 格式），任一冲突整批拒绝（§9.5、§14.2）。
    pub fn batch_update(
        &self,
        workspace_path: &str,
        updates: &[(ProjectDocumentType, i64, String, String)],
    ) -> Result<Vec<ProjectDocumentUpdateResultItem>, BusinessError> {
        let (binding, paths) = self.resolve_binding(workspace_path)?;
        self.with_workspace_sync_lock(&paths, || self.batch_update_locked(&binding, &paths, updates))
    }

    /// 在工作空间同步锁内完成批量校验、文件替换、数据库提交与索引刷新。
    fn batch_update_locked(
        &self,
        binding: &ProjectBinding,
        paths: &WorkspacePaths,
        updates: &[(ProjectDocumentType, i64, String, String)],
    ) -> Result<Vec<ProjectDocumentUpdateResultItem>, BusinessError> {
        // 先同步：外部修改会推进镜像版本，使过期 expected_version 立即冲突。
        let outcome = self.sync_workspace_unlocked(&binding.id, paths)?;
        if outcome.status != status::ACTIVE {
            return Err(BusinessError::with_message(
                ErrorCode::ProjectDocumentPromotionIncomplete,
                format!("项目文档尚未进入可用状态（{}），暂不能批量更新", outcome.status),
            ));
        }
        if updates.is_empty() {
            return Err(BusinessError::with_message(
                ErrorCode::InvalidArgument,
                "批量更新至少包含一份文档",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (kind, _, _, _) in updates {
            if !seen.insert(kind.as_str()) {
                return Err(BusinessError::with_message(
                    ErrorCode::ProjectDocumentTypeInvalid,
                    format!("批量更新包含重复文档类型：{}", kind.as_str()),
                ));
            }
        }
        // 阶段一：全部校验（版本 + 内容）。
        let mut connection = self.database.open()?;
        let mut validated: Vec<(ProjectDocumentType, i64, String, String, ProjectDocumentItem)> = Vec::new();
        for (kind, expected_version, content, summary) in updates {
            let current = self.get_document(&binding.id, *kind).map_err(|_| {
                BusinessError::with_message(
                    ErrorCode::ProjectDocumentNotFound,
                    format!("文档不存在：{}", kind.file_name()),
                )
            })?;
            if current.version != *expected_version {
                return Err(version_conflict(
                    *kind,
                    &current.content,
                    current.version,
                    &current.checksum,
                ));
            }
            let rendered = render_document(*kind, content);
            parse_document(*kind, &rendered)?;
            if summary.trim().is_empty() {
                return Err(BusinessError::with_message(
                    ErrorCode::InvalidArgument,
                    format!("「{}」缺少变更摘要（change_summary）", kind.display_name()),
                ));
            }
            validated.push((*kind, *expected_version, rendered, summary.trim().to_string(), current));
        }
        // 阶段二：先持久化恢复日志，再逐份原子替换文件。
        let journal = BatchUpdateJournal {
            project_id: binding.id.clone(),
            entries: validated
                .iter()
                .map(|(kind, _, rendered, _, current)| BatchUpdateJournalEntry {
                    document_type: *kind,
                    previous_content: current.content.clone(),
                    next_content: rendered.clone(),
                    next_checksum: document_checksum(rendered),
                    next_version: current.version + 1,
                })
                .collect(),
        };
        self.write_batch_update_journal(paths, &journal)?;
        for (kind, _, rendered, _, _) in &validated {
            if let Err(write_error) = atomic_write(&paths.document_path(*kind), rendered) {
                self.recover_batch_update(&binding.id, paths)?;
                return Err(write_error);
            }
        }
        // 阶段三：镜像更新与版本推进在同一数据库事务中提交。
        let now_text = format_storage_time(self.clock.now_utc());
        let database_result = (|| -> Result<Vec<ProjectDocumentUpdateResultItem>, BusinessError> {
            let transaction = connection.transaction().map_err(map_sqlite_error)?;
            let mut results = Vec::new();
            for (kind, expected_version, rendered, _, current) in &validated {
                let checksum = document_checksum(rendered);
                let changed = transaction
                    .execute(
                        "UPDATE project_document \
                         SET previous_content=$prev_content,previous_checksum=$prev_checksum,previous_version=$prev_version, \
                             content=$content,checksum=$checksum,version=version+1,sync_status='SYNCED',updated_at=$updated \
                         WHERE project_id=$pid AND document_type=$type AND version=$expected;",
                        params![
                            current.content,
                            current.checksum,
                            current.version,
                            rendered,
                            checksum,
                            now_text,
                            binding.id,
                            kind.as_str(),
                            expected_version,
                        ],
                    )
                    .map_err(map_sqlite_error)?;
                if changed != 1 {
                    return Err(BusinessError::new(ErrorCode::ProjectDocumentVersionConflict));
                }
                results.push(ProjectDocumentUpdateResultItem {
                    document_type: *kind,
                    version: current.version + 1,
                    checksum,
                    updated_at: now_text.clone(),
                });
            }
            transaction.commit().map_err(map_sqlite_error)?;
            Ok(results)
        })();
        let results = match database_result {
            Ok(results) => results,
            Err(error) => {
                self.recover_batch_update(&binding.id, paths)?;
                return Err(error);
            }
        };
        remove_if_exists(&paths.batch_update_journal_path())?;
        for (kind, _, rendered, _, _) in &validated {
            self.refresh_document_indexes(&binding.id, *kind, rendered)?;
        }
        self.remember_workspace_path(&binding.id, paths)?;
        Ok(results)
    }

    /// 写入批量更新恢复日志，确保任何正式文件变化前已有完整旧/新快照。
    fn write_batch_update_journal(
        &self,
        paths: &WorkspacePaths,
        journal: &BatchUpdateJournal,
    ) -> Result<(), BusinessError> {
        let content = serde_json::to_string(journal).map_err(|error| {
            BusinessError::with_message(
                ErrorCode::InternalError,
                format!("序列化项目文档批量更新日志失败：{error}"),
            )
        })?;
        atomic_write(&paths.batch_update_journal_path(), &content)
    }

    /// 恢复中断的批量更新：数据库已提交则补齐新文件，否则恢复全部旧文件。
    fn recover_batch_update(&self, project_id: &str, paths: &WorkspacePaths) -> Result<(), BusinessError> {
        let Some(content) = read_text_if_exists(&paths.batch_update_journal_path())? else {
            return Ok(());
        };
        let journal: BatchUpdateJournal = serde_json::from_str(&content).map_err(|error| {
            BusinessError::with_message(ErrorCode::InternalError, format!("项目文档批量更新日志损坏：{error}"))
        })?;
        if journal.project_id != project_id {
            return Err(BusinessError::with_message(
                ErrorCode::InternalError,
                "项目文档批量更新日志与当前项目不匹配",
            ));
        }
        let mirrors = load_document_mirrors(&self.database.open()?, project_id)?;
        let database_committed = journal.entries.iter().all(|entry| {
            mirrors
                .get(&entry.document_type)
                .is_some_and(|mirror| mirror.version == entry.next_version && mirror.checksum == entry.next_checksum)
        });
        for entry in &journal.entries {
            let target_content = if database_committed {
                &entry.next_content
            } else {
                &entry.previous_content
            };
            atomic_write(&paths.document_path(entry.document_type), target_content)?;
        }
        remove_if_exists(&paths.batch_update_journal_path())?;
        Ok(())
    }

    /// 更新项目文档本地全文索引，并按项目开关排队远程向量任务。
    fn refresh_document_indexes(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
        content: &str,
    ) -> Result<(), BusinessError> {
        let connection = self.database.open()?;
        let content_tokens = tokenize(content);
        connection
            .execute(
                "DELETE FROM project_document_fts WHERE project_id=$pid AND document_type=$type;",
                params![project_id, document_type.as_str()],
            )
            .map_err(map_sqlite_error)?;
        connection
            .execute(
                "INSERT INTO project_document_fts(project_id,document_type,content_tokens) VALUES($pid,$type,$content);",
                params![project_id, document_type.as_str(), content_tokens],
            )
            .map_err(map_sqlite_error)?;
        let enabled: bool = connection
            .query_row(
                "SELECT project_document_embedding_enabled FROM project WHERE id=$id;",
                params![project_id],
                |row| Ok(row.get::<_, i64>(0)? != 0),
            )
            .optional()
            .map_err(map_sqlite_error)?
            .unwrap_or(false);
        if enabled {
            queue_document_embedding(
                &connection,
                project_id,
                document_type,
                &format_storage_time(self.clock.now_utc()),
            )?;
        }
        Ok(())
    }

    /// 单文件自动修复（§15.2）：保留正文恢复 YAML；正文不可提取时从镜像恢复整份文件。
    pub fn repair_document(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
    ) -> Result<ProjectDocumentItem, BusinessError> {
        let (workspace_path, _) = self.get_project_settings(project_id)?;
        let paths = self.require_workspace_paths(project_id, workspace_path.as_deref())?;
        self.with_workspace_sync_lock(&paths, || {
            self.repair_document_locked(project_id, document_type, &paths)
        })
    }

    /// 在工作空间同步锁内修复单份文档并刷新镜像与索引。
    fn repair_document_locked(
        &self,
        project_id: &str,
        document_type: ProjectDocumentType,
        paths: &WorkspacePaths,
    ) -> Result<ProjectDocumentItem, BusinessError> {
        let path = paths.document_path(document_type);
        let connection = self.database.open()?;
        let current = self.get_document(project_id, document_type)?;
        let content = read_text_if_exists(&path)?;
        let now_text = format_storage_time(self.clock.now_utc());
        let Some(content) = content else {
            // 文件缺失：从镜像恢复完整文件。
            atomic_write(&path, &current.content)?;
            return self.get_document(project_id, document_type);
        };
        if parse_document(document_type, &content).is_ok() {
            // 格式本来正确：仅补同步状态。
            return self.get_document(project_id, document_type);
        }
        match extract_body_lossy(&content) {
            Some(body) => {
                // 保留正文，按文件名恢复正确的最小 YAML（§15.2 顺序）。
                let repaired = render_document(document_type, &body);
                parse_document(document_type, &repaired)?;
                atomic_write(&path, &repaired)?;
                let checksum = document_checksum(&repaired);
                let updated = connection
                    .execute(
                        "UPDATE project_document \
                         SET previous_content=$prev_content,previous_checksum=$prev_checksum,previous_version=$prev_version, \
                             content=$content,checksum=$checksum,version=version+1,sync_status='SYNCED',updated_at=$updated \
                         WHERE project_id=$pid AND document_type=$type AND version=$expected;",
                        params![
                            current.content,
                            current.checksum,
                            current.version,
                            repaired,
                            checksum,
                            now_text,
                            project_id,
                            document_type.as_str(),
                            current.version
                        ],
                    )
                    .map_err(map_sqlite_error)?;
                if updated != 1 {
                    return Err(BusinessError::new(ErrorCode::ProjectDocumentVersionConflict));
                }
                drop(connection);
                self.refresh_document_indexes(project_id, document_type, &repaired)?;
            }
            None => {
                // 正文无法可靠提取：从镜像恢复完整文件。
                atomic_write(&path, &current.content)?;
                let updated = connection
                    .execute(
                        "UPDATE project_document SET sync_status='SYNCED',updated_at=$updated \
                         WHERE project_id=$pid AND document_type=$type;",
                        params![now_text, project_id, document_type.as_str()],
                    )
                    .map_err(map_sqlite_error)?;
                if updated != 1 {
                    return Err(BusinessError::new(ErrorCode::ProjectDocumentVersionConflict));
                }
                drop(connection);
                self.refresh_document_indexes(project_id, document_type, &current.content)?;
            }
        }
        self.get_document(project_id, document_type)
    }

    // -----------------------------------------------------------------------
    // 总览与交接（§9.2、§21.1）
    // -----------------------------------------------------------------------

    /// 项目文档状态总览（桌面端入口）。
    pub fn overview(&self, project_id: &str) -> Result<memory_domain::ProjectDocumentOverview, BusinessError> {
        let connection = self.database.open()?;
        let (name, workspace_path, embedding_enabled) = connection
            .query_row(
                "SELECT name,project_document_workspace_path,project_document_embedding_enabled \
                 FROM project WHERE id=$id;",
                params![project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()
            .map_err(map_sqlite_error)?
            .ok_or_else(|| BusinessError::new(ErrorCode::ProjectNotFound))?;
        drop(connection);
        let (status, warnings, documents, drafts, last_synced_at) =
            if let Some(path) = workspace_path.as_deref().filter(|value| !value.trim().is_empty()) {
                match WorkspacePaths::resolve(path) {
                    Ok(paths) => {
                        let outcome = self.sync_workspace(project_id, &paths)?;
                        let drafts = self.list_drafts_by_project(project_id)?;
                        let last = outcome.states.iter().map(|state| state.updated_at.clone()).max();
                        (
                            outcome.status.to_string(),
                            outcome.warnings,
                            outcome.states,
                            drafts,
                            last,
                        )
                    }
                    Err(error) => (
                        status::NOT_INITIALIZED.to_string(),
                        vec![error.message],
                        Vec::new(),
                        Vec::new(),
                        None,
                    ),
                }
            } else {
                let drafts = self.list_drafts_by_project(project_id)?;
                let status = if drafts.is_empty() {
                    status::NOT_INITIALIZED.to_string()
                } else {
                    status::DRAFT_PENDING_REVIEW.to_string()
                };
                (status, Vec::new(), Vec::new(), drafts, None)
            };
        Ok(memory_domain::ProjectDocumentOverview {
            project_id: project_id.to_string(),
            project_name: name,
            workspace_path: workspace_path.filter(|value| !value.trim().is_empty()),
            status,
            drafts,
            documents,
            embedding_enabled,
            has_residual_drafts: !warnings.is_empty() && warnings.iter().any(|warning| warning.contains("残留")),
            last_synced_at,
        })
    }

    /// 项目交接：读取、校验并组装预算内的项目现场（§9.2）。
    pub fn handoff(&self, request: &ProjectHandoffRequest) -> Result<ProjectHandoffResult, BusinessError> {
        let (binding, paths) = self.resolve_binding(&request.workspace_path)?;
        let outcome = self.sync_workspace(&binding.id, &paths)?;
        self.remember_workspace_path(&binding.id, &paths)?;
        self.register_watcher(&binding.id, &paths)?;

        let read_body = |kind| -> Option<String> {
            outcome
                .mirrors
                .get(&kind)
                .and_then(|mirror| parse_document(kind, &mirror.content).ok())
                .map(|parsed| parsed.body)
        };
        let context_text = truncate_chars(
            &read_body(ProjectDocumentType::Context).unwrap_or_default(),
            request.context_max_chars,
        );
        let current_status_text = truncate_chars(
            &read_body(ProjectDocumentType::CurrentStatus).unwrap_or_default(),
            request.current_status_max_chars,
        );
        let decisions_body = read_body(ProjectDocumentType::Decisions).unwrap_or_default();
        let active_decisions_text = first_entries(
            &extract_section(&decisions_body, "有效决策").unwrap_or_default(),
            request.active_decision_max_count,
        );
        let problems_body = read_body(ProjectDocumentType::Problems).unwrap_or_default();
        let active_problem_ids = collect_problem_ids(&extract_section(&problems_body, "当前问题").unwrap_or_default());
        let active_problems_text = first_entries(
            &extract_section(&problems_body, "当前问题").unwrap_or_default(),
            request.active_problem_max_count,
        );
        let resolved_problems_text = first_entries(
            &extract_section(&problems_body, "已解决问题").unwrap_or_default(),
            request.resolved_problem_max_count,
        );
        let changelog_body = read_body(ProjectDocumentType::Changelog).unwrap_or_default();
        let recent_changelog_text = first_h2_entries(&changelog_body, request.recent_changelog_max_count);

        let related_cards = self.related_conclusion_cards(
            &binding.id,
            &active_problem_ids,
            request.related_conclusion_card_max_count,
            request.conclusion_card_max_chars,
        )?;
        let drafts = self.list_drafts_by_project(&binding.id)?;
        let _ = &outcome.mirrors;
        Ok(ProjectHandoffResult {
            status: outcome.status.to_string(),
            project_id: Some(binding.id),
            project_name: Some(binding.name),
            read_only: false,
            warnings: outcome.warnings,
            documents: outcome.states,
            context_text,
            active_decisions_text,
            current_status_text,
            active_problems_text,
            resolved_problems_text,
            recent_changelog_text,
            related_conclusion_cards: related_cards,
            drafts,
        })
    }

    /// 在交接成功后注册当前会话的监听租约；同一进程同一工作空间复用底层线程。
    fn register_watcher(&self, project_id: &str, paths: &WorkspacePaths) -> Result<(), BusinessError> {
        let database = self.database.clone();
        let clock = Arc::clone(&self.clock);
        let ids = Arc::clone(&self.ids);
        let project_id = project_id.to_string();
        let paths_for_callback = paths.clone();
        let callback = Arc::new(move || {
            let service = ProjectDocumentService::new(database.clone(), Arc::clone(&clock), Arc::clone(&ids));
            if let Err(error) = service.sync_workspace(&project_id, &paths_for_callback) {
                eprintln!("[project-document-watcher] 自动同步失败：{}", error.message);
            }
        });
        let lease = project_document_watcher::register(paths, callback)?;
        let mut watcher = self.watcher.lock().expect("项目文档监听租约锁已中毒");
        *watcher = Some(lease);
        Ok(())
    }

    /// 与当前问题相关的结论卡片（优先匹配问题编号，其次按重要度）。
    fn related_conclusion_cards(
        &self,
        project_id: &str,
        active_problem_ids: &[String],
        max_count: i64,
        max_chars: i64,
    ) -> Result<Vec<ConclusionCardBrief>, BusinessError> {
        if max_count <= 0 {
            return Ok(Vec::new());
        }
        let connection = self.database.open()?;
        let mut statement = connection
            .prepare(
                "SELECT cc.memory_id,cc.problem_id,m.title,m.content,m.importance,m.updated_at \
                 FROM conclusion_card cc INNER JOIN memory m ON m.id=cc.memory_id \
                 WHERE m.project_id=$pid AND m.status='Active' \
                 ORDER BY m.importance DESC,m.updated_at DESC;",
            )
            .map_err(map_sqlite_error)?;
        let rows = statement
            .query_map(params![project_id], |row| {
                Ok(ConclusionCardBrief {
                    memory_id: row.get(0)?,
                    problem_id: row.get(1)?,
                    title: row.get(2)?,
                    content: row.get(3)?,
                    importance: row.get(4)?,
                    resolved_at: row.get(5)?,
                })
            })
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        let mut matched: Vec<ConclusionCardBrief> = Vec::new();
        let mut others: Vec<ConclusionCardBrief> = Vec::new();
        for card in rows {
            if card
                .problem_id
                .as_deref()
                .is_some_and(|problem| active_problem_ids.iter().any(|active| active == problem))
            {
                matched.push(card);
            } else {
                others.push(card);
            }
        }
        matched.extend(others);
        matched.truncate(max_count.max(0) as usize);
        for card in &mut matched {
            card.content = truncate_chars(&card.content, max_chars);
        }
        Ok(matched)
    }
}

// ---------------------------------------------------------------------------
// 行读取与写入辅助
// ---------------------------------------------------------------------------

fn read_draft_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectDocumentDraftItem> {
    let type_text: String = row.get(2)?;
    let document_type = ProjectDocumentType::parse(&type_text).ok_or(rusqlite::Error::InvalidColumnType(
        2,
        type_text,
        rusqlite::types::Type::Text,
    ))?;
    Ok(ProjectDocumentDraftItem {
        id: row.get(0)?,
        project_id: row.get(1)?,
        document_type,
        relative_path: row.get(3)?,
        content: row.get(4)?,
        checksum: row.get(5)?,
        version: row.get(6)?,
        review_status: row.get(7)?,
        approved_version: row.get(8)?,
        last_change_reason: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn read_document_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectDocumentItem> {
    let type_text: String = row.get(2)?;
    let document_type = ProjectDocumentType::parse(&type_text).ok_or(rusqlite::Error::InvalidColumnType(
        2,
        type_text,
        rusqlite::types::Type::Text,
    ))?;
    Ok(ProjectDocumentItem {
        id: row.get(0)?,
        project_id: row.get(1)?,
        document_type,
        relative_path: row.get(3)?,
        content: row.get(4)?,
        checksum: row.get(5)?,
        version: row.get(6)?,
        previous_version: row.get(7)?,
        embedding_enabled: row.get::<_, i64>(8)? != 0,
        sync_status: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn insert_draft_row(
    transaction: &rusqlite::Transaction<'_>,
    id: &str,
    project_id: &str,
    document_type: ProjectDocumentType,
    content: &str,
    change_reason: Option<&str>,
    now_text: &str,
) -> Result<(), BusinessError> {
    transaction
        .execute(
            "INSERT INTO project_document_draft( \
                 id,project_id,document_type,relative_path,content,checksum,version, \
                 review_status,approved_version,last_change_reason,created_at,updated_at) \
             VALUES($id,$pid,$type,$path,$content,$checksum,1,'PENDING_REVIEW',NULL,$reason,$now,$now);",
            params![
                id,
                project_id,
                document_type.as_str(),
                document_type.file_name(),
                content,
                document_checksum(content),
                change_reason,
                now_text,
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn upsert_document_mirror(
    transaction: &rusqlite::Transaction<'_>,
    id: &str,
    project_id: &str,
    document_type: ProjectDocumentType,
    content: &str,
    now_text: &str,
) -> Result<(), BusinessError> {
    transaction
        .execute(
            "INSERT INTO project_document( \
                 id,project_id,document_type,relative_path,content,checksum,version, \
                 previous_content,previous_checksum,previous_version,embedding_enabled,sync_status,created_at,updated_at) \
             VALUES($id,$pid,$type,$path,$content,$checksum,1,NULL,NULL,NULL,$embedding,'SYNCED',$now,$now) \
             ON CONFLICT(project_id,document_type) DO UPDATE SET \
                 content=excluded.content,checksum=excluded.checksum,version=excluded.version, \
                 sync_status='SYNCED',updated_at=excluded.updated_at;",
            params![
                id,
                project_id,
                document_type.as_str(),
                document_type.file_name(),
                content,
                document_checksum(content),
                0,
                now_text,
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn update_mirror_from_file(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    document_type: ProjectDocumentType,
    content: &str,
    now_text: &str,
) -> Result<(), BusinessError> {
    transaction
        .execute(
            "UPDATE project_document \
             SET previous_content=content,previous_checksum=checksum,previous_version=version, \
                 content=$content,checksum=$checksum,version=version+1,sync_status='SYNCED',updated_at=$updated \
             WHERE project_id=$pid AND document_type=$type;",
            params![
                content,
                document_checksum(content),
                now_text,
                project_id,
                document_type.as_str()
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn set_document_sync_status(
    transaction: &rusqlite::Transaction<'_>,
    project_id: &str,
    document_type: ProjectDocumentType,
    sync_status: &str,
) -> Result<(), BusinessError> {
    transaction
        .execute(
            "UPDATE project_document SET sync_status=$status WHERE project_id=$pid AND document_type=$type;",
            params![sync_status, project_id, document_type.as_str()],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

/// 为项目全部正式文档创建去重的 Embedding 任务。
fn queue_project_document_embeddings(
    connection: &Connection,
    project_id: &str,
    now_text: &str,
) -> Result<(), BusinessError> {
    connection
        .execute(
            "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
             SELECT lower(hex(randomblob(16))),'EMBED_PROJECT_DOCUMENT',d.id,'PENDING',0,$now,NULL,NULL,$now,$now \
             FROM project_document d WHERE d.project_id=$pid \
             AND NOT EXISTS(SELECT 1 FROM background_task t WHERE t.task_type='EMBED_PROJECT_DOCUMENT' AND t.target_id=d.id AND t.status IN ('PENDING','RUNNING'));",
            params![now_text, project_id],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

/// 为一份正式文档创建去重的 Embedding 任务。
fn queue_document_embedding(
    connection: &Connection,
    project_id: &str,
    document_type: ProjectDocumentType,
    now_text: &str,
) -> Result<(), BusinessError> {
    connection
        .execute(
            "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
             SELECT lower(hex(randomblob(16))),'EMBED_PROJECT_DOCUMENT',d.id,'PENDING',0,$now,NULL,NULL,$now,$now \
             FROM project_document d WHERE d.project_id=$pid AND d.document_type=$type \
             AND NOT EXISTS(SELECT 1 FROM background_task t WHERE t.task_type='EMBED_PROJECT_DOCUMENT' AND t.target_id=d.id AND t.status IN ('PENDING','RUNNING'));",
            params![now_text, project_id, document_type.as_str()],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

fn load_document_mirrors(
    connection: &Connection,
    project_id: &str,
) -> Result<BTreeMap<ProjectDocumentType, DocumentMirror>, BusinessError> {
    let mut statement = connection
        .prepare("SELECT document_type,content,checksum,version FROM project_document WHERE project_id=$pid;")
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params![project_id], |row| {
            let type_text: String = row.get(0)?;
            Ok((
                type_text,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(map_sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite_error)?;
    let mut mirrors = BTreeMap::new();
    for (type_text, content, checksum, version) in rows {
        if let Some(kind) = ProjectDocumentType::parse(&type_text) {
            mirrors.insert(
                kind,
                DocumentMirror {
                    content,
                    checksum,
                    version,
                },
            );
        }
    }
    Ok(mirrors)
}

fn count_document_rows(connection: &Connection, project_id: &str) -> Result<i64, BusinessError> {
    connection
        .query_row(
            "SELECT count(*) FROM project_document WHERE project_id=$pid;",
            params![project_id],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)
}

fn formal_files_present(paths: &WorkspacePaths) -> bool {
    ALL_PROJECT_DOCUMENT_TYPES
        .into_iter()
        .any(|kind| paths.document_path(kind).exists())
}

fn read_all_document_files(
    paths: &WorkspacePaths,
) -> Result<Vec<(ProjectDocumentType, Option<String>)>, BusinessError> {
    let mut result = Vec::new();
    for kind in ALL_PROJECT_DOCUMENT_TYPES {
        result.push((kind, read_text_if_exists(&paths.document_path(kind))?));
    }
    Ok(result)
}

fn read_all_draft_files(paths: &WorkspacePaths) -> Result<Vec<(ProjectDocumentType, String)>, BusinessError> {
    let mut result = Vec::new();
    for kind in ALL_PROJECT_DOCUMENT_TYPES {
        if let Some(content) = read_text_if_exists(&paths.draft_path(kind))? {
            result.push((kind, content));
        }
    }
    Ok(result)
}

/// 版本冲突错误：携带当前版本、校验和与内容引用（§14.1）。
fn version_conflict(document_type: ProjectDocumentType, content: &str, version: i64, checksum: &str) -> BusinessError {
    BusinessError::new(ErrorCode::ProjectDocumentVersionConflict).with_details(serde_json::json!({
        "documentType": document_type.as_str(),
        "currentVersion": version,
        "currentChecksum": checksum,
        "contentPreview": content.chars().take(2000).collect::<String>(),
    }))
}

/// 校验五份草稿集合：齐全、类型唯一、正文非空。
fn validate_document_set(documents: &[(ProjectDocumentType, String)]) -> Result<(), BusinessError> {
    if documents.len() != ALL_PROJECT_DOCUMENT_TYPES.len() {
        return Err(BusinessError::new(ErrorCode::ProjectDocumentDraftIncomplete));
    }
    let mut seen = std::collections::BTreeSet::new();
    for (kind, body) in documents {
        if !seen.insert(kind.as_str()) {
            return Err(BusinessError::with_message(
                ErrorCode::ProjectDocumentTypeInvalid,
                format!("草稿集合包含重复文档类型：{}", kind.as_str()),
            ));
        }
        if body.trim().is_empty() {
            return Err(BusinessError::with_message(
                ErrorCode::ProjectDocumentFormatInvalid,
                format!("「{}」正文为空", kind.display_name()),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 交接文本裁剪（§9.2 预算）
// ---------------------------------------------------------------------------

/// 截断到最大字符数（按 char 计数），超限时追加截断标记。
fn truncate_chars(text: &str, max_chars: i64) -> String {
    if max_chars <= 0 {
        return String::new();
    }
    let limit = max_chars as usize;
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let truncated: String = text.chars().take(limit).collect();
    format!("{truncated}\n…（已按字符预算截断）")
}

/// 提取 `## <heading>` 小节内容（到下一个同级或更高级标题为止）。
fn extract_section(body: &str, heading: &str) -> Option<String> {
    let target = format!("## {heading}");
    let mut lines = body.lines();
    let mut collecting = false;
    let mut collected: Vec<&str> = Vec::new();
    for line in lines.by_ref() {
        if !collecting {
            if line.trim() == target || line.trim() == format!("{target} ") {
                collecting = true;
            }
        } else if line.starts_with("## ") || (line.starts_with("# ") && !line.starts_with("## ")) {
            break;
        } else {
            collected.push(line);
        }
    }
    if !collecting {
        return None;
    }
    let text = collected.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// 取前 N 个条目（条目以 `### ` 标题分界；无标题时整体作为一个条目）。
fn first_entries(section: &str, max_count: i64) -> String {
    if max_count <= 0 {
        return String::new();
    }
    let entries = split_entries(section);
    let limited: Vec<&str> = entries
        .iter()
        .take(max_count.max(0) as usize)
        .map(String::as_str)
        .collect();
    if limited.len() < entries.len() {
        return format!("{}\n…（已按条数预算截断）", limited.join("\n\n").trim_end());
    }
    limited.join("\n\n")
}

/// 取前 N 个 `## ` 二级小节（Changelog 按日期倒序的条目单位；跳过一级标题）。
fn first_h2_entries(body: &str, max_count: i64) -> String {
    if max_count <= 0 {
        return String::new();
    }
    let mut entries: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in body.lines() {
        if line.starts_with("## ") {
            if !current.trim().is_empty() {
                entries.push(current.trim().to_string());
            }
            current = line.to_string();
        } else {
            current.push('\n');
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        entries.push(current.trim().to_string());
    }
    // 跳过一级标题（如「# 变更记录」），只保留二级小节条目。
    let entries: Vec<String> = entries.into_iter().filter(|entry| entry.starts_with("## ")).collect();
    let limited: Vec<&str> = entries
        .iter()
        .take(max_count.max(0) as usize)
        .map(String::as_str)
        .collect();
    if limited.len() < entries.len() {
        return format!("{}\n…（已按条数预算截断）", limited.join("\n\n").trim_end());
    }
    limited.join("\n\n")
}

fn split_entries(section: &str) -> Vec<String> {
    if !section.contains("\n### ") && !section.starts_with("### ") {
        return vec![section.trim().to_string()];
    }
    let mut entries: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in section.lines() {
        if line.starts_with("### ") {
            if !current.trim().is_empty() {
                entries.push(current.trim().to_string());
            }
            current = line.to_string();
        } else {
            current.push('\n');
            current.push_str(line);
        }
    }
    if !current.trim().is_empty() {
        entries.push(current.trim().to_string());
    }
    entries
}

/// 从问题小节收集 `PROB-日期-序号` 编号（结论卡片关联用）。
fn collect_problem_ids(section: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for line in section.lines() {
        let trimmed = line.trim_start_matches(['#', ' ', '*', '-']);
        if let Some(id) = trimmed.strip_prefix("PROB-") {
            let id: String = format!(
                "PROB-{}",
                id.chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect::<String>()
            );
            let id = id.trim_end_matches('-').to_string();
            if id.len() > "PROB-0-0".len() {
                ids.push(id);
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use chrono::{TimeZone, Utc};
    use memory_domain::ProjectHandoffRequest;

    struct TestContext {
        service: ProjectDocumentService,
        database: Database,
        workspace: tempfile::TempDir,
        /// 数据库所在临时目录（保持存活，防止目录被提前清理）。
        _directory: tempfile::TempDir,
        project_id: String,
    }

    fn context() -> TestContext {
        let temp = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let database = Database::new(database_path);
        let clock = Arc::new(FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 21, 8, 0, 0).unwrap()));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=40)
                .map(|index| format!("{index:08}-{index:04}-4{index:03}-8{index:03}-{index:012}"))
                .collect(),
        ));
        let service = ProjectDocumentService::new(database.clone(), clock, ids);
        // 建项目并绑定工作空间。
        let connection = database.open().unwrap();
        let key = calculate_key(&normalize_identifier(workspace.path().to_str().unwrap()).unwrap());
        connection
            .execute(
                "INSERT INTO project(id,name,description,color,is_archived,workspace_key,workspace_label,created_at,updated_at) \
                 VALUES('11111111-1111-4111-8111-111111111111','测试项目','','#238f7a',0,$key,$label,'2026-08-21T08:00:00+00:00','2026-08-21T08:00:00+00:00');",
                params![key, workspace.path().to_str().unwrap()],
            )
            .unwrap();
        drop(connection);
        TestContext {
            service,
            database,
            workspace,
            _directory: temp,
            project_id: "11111111-1111-4111-8111-111111111111".to_string(),
        }
    }

    fn workspace_path(context: &TestContext) -> String {
        context.workspace.path().to_str().unwrap().to_string()
    }

    fn five_documents() -> Vec<(ProjectDocumentType, String)> {
        ALL_PROJECT_DOCUMENT_TYPES
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    format!("# {}\n\n{}初始内容。", kind.display_name(), kind.display_name()),
                )
            })
            .collect()
    }

    fn approve_all(context: &TestContext) {
        for kind in ALL_PROJECT_DOCUMENT_TYPES {
            context.service.approve_draft(&context.project_id, kind).unwrap();
        }
    }

    #[test]
    fn draft_lifecycle_create_update_approve_promote() {
        let context = context();
        let drafts = context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), Some("初始创建"))
            .unwrap();
        assert_eq!(drafts.len(), 5);
        assert!(drafts.iter().all(|draft| draft.review_status == "PENDING_REVIEW"));
        // 重复创建：返回现有，不重复。
        let again = context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        assert_eq!(again.len(), 5);

        // 更新一份：版本递增 + 回到待审核。
        let context_draft = context
            .service
            .get_draft(&context.project_id, ProjectDocumentType::Context)
            .unwrap();
        let updated = context
            .service
            .update_draft(
                &context.project_id,
                ProjectDocumentType::Context,
                context_draft.version,
                "# 项目背景\n\n更新后的内容。",
                "AI 补充运行环境",
            )
            .unwrap();
        assert_eq!(updated.version, 2);
        assert_eq!(updated.review_status, "PENDING_REVIEW");

        // 乐观锁冲突。
        let error = context
            .service
            .update_draft(
                &context.project_id,
                ProjectDocumentType::Context,
                1,
                "# 项目背景\n\n过期版本。",
                "过期",
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentVersionConflict);
        assert_eq!(error.details["currentVersion"], 2);

        // 逐份批准。
        for kind in ALL_PROJECT_DOCUMENT_TYPES {
            context.service.approve_draft(&context.project_id, kind).unwrap();
        }
        let approved = context.service.list_drafts_by_project(&context.project_id).unwrap();
        assert!(approved.iter().all(|draft| draft.review_status == "APPROVED"));

        // 晋升。
        let documents = context.service.promote_drafts(&context.project_id).unwrap();
        assert_eq!(documents.len(), 5);
        assert!(
            documents
                .iter()
                .all(|doc| doc.version == 1 && doc.sync_status == "SYNCED")
        );
        // 正式文件存在；草稿目录删除；草稿行删除。
        let memstack_dir = context.workspace.path().join(".memstack");
        assert!(memstack_dir.join("01_CONTEXT.md").exists());
        assert!(!memstack_dir.join("drafts").exists());
        assert!(
            context
                .service
                .list_drafts_by_project(&context.project_id)
                .unwrap()
                .is_empty()
        );
        // 正式存在后拒绝再建草稿。
        let error = context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentAlreadyActive);
    }

    #[test]
    fn editing_approved_draft_revokes_approval() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        context
            .service
            .approve_draft(&context.project_id, ProjectDocumentType::Decisions)
            .unwrap();
        let draft = context
            .service
            .get_draft(&context.project_id, ProjectDocumentType::Decisions)
            .unwrap();
        let updated = context
            .service
            .update_draft(
                &context.project_id,
                ProjectDocumentType::Decisions,
                draft.version,
                "# 项目决策\n\n新增决策。",
                "AI 更新",
            )
            .unwrap();
        assert_eq!(updated.review_status, "PENDING_REVIEW");
        // approved_version 保留历史值，但不等于当前 version → 不能晋升。
        assert_ne!(updated.approved_version, Some(updated.version));
        let error = context.service.promote_drafts(&context.project_id).unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentDraftIncomplete);
    }

    #[test]
    fn promotion_recovery_completes_partial_files() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        // 模拟中断：晋升前只写了一个正式文件。
        let memstack_dir = context.workspace.path().join(".memstack");
        std::fs::create_dir_all(&memstack_dir).unwrap();
        std::fs::write(
            memstack_dir.join("01_CONTEXT.md"),
            render_document(ProjectDocumentType::Context, "# 项目背景\n\n部分晋升内容。"),
        )
        .unwrap();
        // 交接触发恢复：用已批准草稿补齐剩余文件。
        let result = context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert_eq!(result.status, "ACTIVE");
        assert!(result.warnings.iter().any(|warning| warning.contains("自动恢复完成")));
        assert!(memstack_dir.join("05_CHANGELOG.md").exists());
    }

    #[test]
    fn external_modification_syncs_and_conflicts() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();

        // 用户外部修改正式文件 → 交接时补同步（版本 +1）。
        let memstack_dir = context.workspace.path().join(".memstack");
        std::fs::write(
            memstack_dir.join("03_CURRENT_STATUS.md"),
            render_document(ProjectDocumentType::CurrentStatus, "# 当前状态\n\n用户手动更新。"),
        )
        .unwrap();
        let result = context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert_eq!(result.status, "ACTIVE");
        assert!(result.warnings.iter().any(|warning| warning.contains("已同步外部修改")));
        let document = context
            .service
            .get_document(&context.project_id, ProjectDocumentType::CurrentStatus)
            .unwrap();
        assert_eq!(document.version, 2);

        // AI 基于过期版本批量更新 → 冲突，整批拒绝。
        let updates = vec![
            (
                ProjectDocumentType::CurrentStatus,
                1,
                "# 当前状态\n\nAI 更新。".to_string(),
                "状态更新".to_string(),
            ),
            (
                ProjectDocumentType::Problems,
                1,
                "# 项目问题\n\nAI 更新。".to_string(),
                "问题更新".to_string(),
            ),
        ];
        let error = context
            .service
            .batch_update(&workspace_path(&context), &updates)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentVersionConflict);
        // 整批拒绝：PROBLEMS 未被写入。
        let problems = context
            .service
            .get_document(&context.project_id, ProjectDocumentType::Problems)
            .unwrap();
        assert!(problems.content.contains("初始内容"));

        // AI 基于新版本批量更新 → 成功，两份文档版本一起推进。
        let updates = vec![
            (
                ProjectDocumentType::CurrentStatus,
                2,
                "# 当前状态\n\nAI 更新。".to_string(),
                "状态更新".to_string(),
            ),
            (
                ProjectDocumentType::Problems,
                1,
                "# 项目问题\n\nAI 更新。".to_string(),
                "问题更新".to_string(),
            ),
        ];
        let results = context
            .service
            .batch_update(&workspace_path(&context), &updates)
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].version, 3);
        assert_eq!(results[1].version, 2);
        let on_disk = std::fs::read_to_string(memstack_dir.join("03_CURRENT_STATUS.md")).unwrap();
        assert!(on_disk.contains("AI 更新。"));
    }

    #[test]
    fn interrupted_batch_without_database_commit_restores_all_previous_files() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        let paths = WorkspacePaths::resolve(&workspace_path(&context)).unwrap();
        let current = context
            .service
            .get_document(&context.project_id, ProjectDocumentType::Context)
            .unwrap();
        let problems = context
            .service
            .get_document(&context.project_id, ProjectDocumentType::Problems)
            .unwrap();
        let next_context = render_document(ProjectDocumentType::Context, "# 项目背景\n\n批量新内容。");
        let next_problems = render_document(ProjectDocumentType::Problems, "# 项目问题\n\n批量新问题。");
        let journal = BatchUpdateJournal {
            project_id: context.project_id.clone(),
            entries: vec![
                BatchUpdateJournalEntry {
                    document_type: ProjectDocumentType::Context,
                    previous_content: current.content.clone(),
                    next_checksum: document_checksum(&next_context),
                    next_content: next_context.clone(),
                    next_version: current.version + 1,
                },
                BatchUpdateJournalEntry {
                    document_type: ProjectDocumentType::Problems,
                    previous_content: problems.content.clone(),
                    next_checksum: document_checksum(&next_problems),
                    next_content: next_problems,
                    next_version: problems.version + 1,
                },
            ],
        };
        context.service.write_batch_update_journal(&paths, &journal).unwrap();
        atomic_write(&paths.document_path(ProjectDocumentType::Context), &next_context).unwrap();

        context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(paths.document_path(ProjectDocumentType::Context)).unwrap(),
            current.content
        );
        assert_eq!(
            std::fs::read_to_string(paths.document_path(ProjectDocumentType::Problems)).unwrap(),
            problems.content
        );
        assert!(!paths.batch_update_journal_path().exists());
    }

    #[test]
    fn interrupted_batch_after_database_commit_completes_all_next_files() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        let paths = WorkspacePaths::resolve(&workspace_path(&context)).unwrap();
        let current = context
            .service
            .get_document(&context.project_id, ProjectDocumentType::CurrentStatus)
            .unwrap();
        let next = render_document(ProjectDocumentType::CurrentStatus, "# 当前状态\n\n数据库已提交。");
        let next_checksum = document_checksum(&next);
        let journal = BatchUpdateJournal {
            project_id: context.project_id.clone(),
            entries: vec![BatchUpdateJournalEntry {
                document_type: ProjectDocumentType::CurrentStatus,
                previous_content: current.content.clone(),
                next_content: next.clone(),
                next_checksum: next_checksum.clone(),
                next_version: current.version + 1,
            }],
        };
        context.service.write_batch_update_journal(&paths, &journal).unwrap();
        context
            .service
            .database
            .open()
            .unwrap()
            .execute(
                "UPDATE project_document SET content=$content,checksum=$checksum,version=version+1 \
                 WHERE project_id=$pid AND document_type=$type;",
                params![
                    next,
                    next_checksum,
                    context.project_id,
                    ProjectDocumentType::CurrentStatus.as_str()
                ],
            )
            .unwrap();

        context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert!(
            std::fs::read_to_string(paths.document_path(ProjectDocumentType::CurrentStatus))
                .unwrap()
                .contains("数据库已提交")
        );
        assert!(!paths.batch_update_journal_path().exists());
    }

    #[test]
    fn active_documents_keep_residual_drafts_for_manual_resolution() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        let paths = WorkspacePaths::resolve(&workspace_path(&context)).unwrap();
        paths.ensure_memstack_dirs().unwrap();
        let residual = render_document(ProjectDocumentType::Context, "# 项目背景\n\n残留草稿。");
        atomic_write(&paths.draft_path(ProjectDocumentType::Context), &residual).unwrap();
        context
            .service
            .database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO project_document_draft( \
                     id,project_id,document_type,relative_path,content,checksum,version,review_status, \
                     approved_version,last_change_reason,created_at,updated_at) \
                 VALUES('residual-draft',$pid,'CONTEXT','01_CONTEXT.md',$content,$checksum,1, \
                        'PENDING_REVIEW',NULL,'恢复测试',$now,$now);",
                params![
                    context.project_id,
                    residual,
                    document_checksum(&residual),
                    "2026-08-21T08:00:00+00:00"
                ],
            )
            .unwrap();

        let handoff = context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert_eq!(handoff.status, status::ACTIVE);
        assert!(handoff.warnings.iter().any(|warning| warning.contains("草稿已保留")));
        assert_eq!(
            context
                .service
                .list_drafts_by_project(&context.project_id)
                .unwrap()
                .len(),
            1
        );
        assert!(paths.draft_path(ProjectDocumentType::Context).exists());
    }

    #[test]
    fn missing_file_restored_from_mirror() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        let memstack_dir = context.workspace.path().join(".memstack");
        std::fs::remove_file(memstack_dir.join("02_DECISIONS.md")).unwrap();
        let result = context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert_eq!(result.status, "ACTIVE");
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("已从数据库镜像恢复"))
        );
        assert!(memstack_dir.join("02_DECISIONS.md").exists());
    }

    #[test]
    fn format_error_does_not_overwrite_user_file_and_repair_recovers() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        let memstack_dir = context.workspace.path().join(".memstack");
        // 用户把 YAML 头改坏但正文仍在。
        std::fs::write(
            memstack_dir.join("01_CONTEXT.md"),
            "---\nbroken yaml!!!\n---\n\n# 项目背景\n\n正文保留。",
        )
        .unwrap();
        let result = context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert_eq!(result.status, "PROMOTING", "格式错误时不得视为 ACTIVE");
        assert!(result.warnings.iter().any(|warning| warning.contains("格式")));
        // 文件未被覆盖。
        let content = std::fs::read_to_string(memstack_dir.join("01_CONTEXT.md")).unwrap();
        assert!(content.contains("broken yaml"));
        // 自动修复：保留正文恢复 YAML。
        let repaired = context
            .service
            .repair_document(&context.project_id, ProjectDocumentType::Context)
            .unwrap();
        assert!(repaired.content.contains("# 项目背景"));
        assert!(repaired.content.contains("正文保留。"));
        let connection = context.database.open().unwrap();
        let repaired_fts: i64 = connection
            .query_row(
                "SELECT count(*) FROM project_document_fts WHERE project_id=$pid AND document_type='CONTEXT' AND project_document_fts MATCH '\"正文\"';",
                params![context.project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repaired_fts, 1, "修复后的正文必须立即刷新本地全文索引");
        drop(connection);
        let after = context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        assert_eq!(after.status, "ACTIVE");
    }

    #[test]
    fn handoff_respects_budgets() {
        let context = context();
        let documents: Vec<(ProjectDocumentType, String)> = vec![
            (
                ProjectDocumentType::Context,
                "# 项目背景\n\n很长的背景内容。".to_string(),
            ),
            (
                ProjectDocumentType::Decisions,
                "# 项目决策\n\n## 有效决策\n\n### DEC-20260821-001\n\n第一项决策。\n\n### DEC-20260821-002\n\n第二项决策。\n\n### DEC-20260821-003\n\n第三项决策。\n\n## 已替代决策\n\n（无）".to_string(),
            ),
            (
                ProjectDocumentType::CurrentStatus,
                "# 当前状态\n\n## 当前阶段\n\n开发中。".to_string(),
            ),
            (
                ProjectDocumentType::Problems,
                "# 项目问题\n\n## 当前问题\n\n### PROB-20260821-001 启动失败\n\n严重程度：高。\n\n## 已解决问题\n\n### PROB-20260820-001 旧问题\n\n已解决摘要。".to_string(),
            ),
            (
                ProjectDocumentType::Changelog,
                "# 变更记录\n\n## 2026-08-21\n\n完成 A。\n\n## 2026-08-20\n\n完成 B。\n\n## 2026-08-19\n\n完成 C。".to_string(),
            ),
        ];
        context
            .service
            .create_drafts(&workspace_path(&context), &documents, None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        let request = ProjectHandoffRequest {
            workspace_path: workspace_path(&context),
            context_max_chars: 1000,
            active_decision_max_count: 2,
            current_status_max_chars: 1000,
            active_problem_max_count: 5,
            resolved_problem_max_count: 3,
            recent_changelog_max_count: 2,
            related_conclusion_card_max_count: 3,
            conclusion_card_max_chars: 500,
        };
        let result = context.service.handoff(&request).unwrap();
        assert_eq!(result.status, "ACTIVE");
        assert!(result.context_text.contains("很长的背景内容"));
        // 决策只取前 2 项。
        assert!(result.active_decisions_text.contains("DEC-20260821-001"));
        assert!(!result.active_decisions_text.contains("DEC-20260821-003"));
        // 问题分当前 / 已解决。
        assert!(result.active_problems_text.contains("PROB-20260821-001"));
        assert!(result.resolved_problems_text.contains("PROB-20260820-001"));
        // Changelog 只取前 2 天。
        assert!(result.recent_changelog_text.contains("2026-08-21"));
        assert!(!result.recent_changelog_text.contains("2026-08-19"));
        // 预算为 0 时对应段为空。
        let zero_request = ProjectHandoffRequest {
            workspace_path: workspace_path(&context),
            context_max_chars: 0,
            active_decision_max_count: 0,
            current_status_max_chars: 0,
            active_problem_max_count: 0,
            resolved_problem_max_count: 0,
            recent_changelog_max_count: 0,
            related_conclusion_card_max_count: 0,
            conclusion_card_max_chars: 0,
        };
        let zero = context.service.handoff(&zero_request).unwrap();
        assert!(zero.context_text.is_empty());
        assert!(zero.active_decisions_text.is_empty());
    }

    #[test]
    fn handoff_follows_workspace_when_parent_directory_changes() {
        let context = context();
        let original_path = context.workspace.path().to_path_buf();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();

        let destination_parent = context._directory.path().join("新磁盘");
        std::fs::create_dir_all(&destination_parent).unwrap();
        let destination = destination_parent.join(original_path.file_name().unwrap());
        std::fs::rename(&original_path, &destination).unwrap();

        let result = context
            .service
            .handoff(&handoff_request(destination.to_str().unwrap()))
            .unwrap();
        assert_eq!(result.project_id.as_deref(), Some(context.project_id.as_str()));
        assert_eq!(result.status, "ACTIVE");
        assert!(destination.join(".memstack/01_CONTEXT.md").exists());
        assert!(!original_path.exists());

        let (remembered_path, _) = context.service.get_project_settings(&context.project_id).unwrap();
        let expected_path = destination.canonicalize().unwrap();
        let expected_display = expected_path
            .to_str()
            .map(|text| text.trim_start_matches(r"\\?\").to_string());
        assert_eq!(remembered_path, expected_display);
    }

    #[test]
    fn unbound_workspace_is_rejected() {
        let context = context();
        let elsewhere = tempfile::tempdir().unwrap();
        let error = context
            .service
            .handoff(&handoff_request(elsewhere.path().to_str().unwrap()))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentWorkspaceUnbound);
    }

    #[test]
    fn incomplete_draft_set_is_rejected() {
        let context = context();
        let partial = vec![(ProjectDocumentType::Context, "# 背景".to_string())];
        let error = context
            .service
            .create_drafts(&workspace_path(&context), &partial, None)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ProjectDocumentDraftIncomplete);
    }

    #[test]
    fn embedding_setting_defaults_off_and_updates() {
        let context = context();
        let (path, enabled) = context.service.get_project_settings(&context.project_id).unwrap();
        assert!(!enabled, "项目文档 Embedding 默认关闭");
        assert!(path.is_none());
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        context
            .service
            .set_embedding_enabled(&context.project_id, true)
            .unwrap();
        let (_, enabled) = context.service.get_project_settings(&context.project_id).unwrap();
        assert!(enabled);
        let document = context
            .service
            .get_document(&context.project_id, ProjectDocumentType::Context)
            .unwrap();
        assert!(document.embedding_enabled, "镜像行开关随项目设置同步");
        // 记录了工作空间路径。
        let (workspace, _) = context.service.get_project_settings(&context.project_id).unwrap();
        assert!(workspace.is_some());
    }

    #[test]
    fn project_document_indexes_and_embedding_tasks_follow_project_setting() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();

        let connection = context.database.open().unwrap();
        let fts_rows: i64 = connection
            .query_row(
                "SELECT count(*) FROM project_document_fts WHERE project_id=$pid;",
                params![context.project_id],
                |row| row.get(0),
            )
            .unwrap();
        let matches: i64 = connection
            .query_row(
                "SELECT count(*) FROM project_document_fts WHERE project_id=$pid AND project_document_fts MATCH '\"初始\"';",
                params![context.project_id],
                |row| row.get(0),
            )
            .unwrap();
        let disabled_tasks: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='EMBED_PROJECT_DOCUMENT';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        assert_eq!(fts_rows, 5, "五份正式文档都必须进入独立 FTS");
        assert_eq!(matches, 5, "项目文档 FTS 必须可按正文词元检索");
        assert_eq!(disabled_tasks, 0, "项目开关默认关闭时不得创建远程向量任务");

        context
            .service
            .set_embedding_enabled(&context.project_id, true)
            .unwrap();
        context
            .service
            .set_embedding_enabled(&context.project_id, true)
            .unwrap();
        let connection = context.database.open().unwrap();
        let enabled_tasks: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='EMBED_PROJECT_DOCUMENT' AND status='PENDING';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(enabled_tasks, 5, "重复开启或刷新不得重复排队");
        connection
            .execute(
                "INSERT INTO project_document_embedding(project_id,document_type,provider,model,dimensions,content_checksum,vector_blob,updated_at) \
                 SELECT project_id,document_type,'TEST','test-model',1,checksum,x'00000000','2026-08-21T08:00:00+00:00' \
                 FROM project_document WHERE project_id=$pid;",
                params![context.project_id],
            )
            .unwrap();
        drop(connection);

        context
            .service
            .set_embedding_enabled(&context.project_id, false)
            .unwrap();
        let connection = context.database.open().unwrap();
        let remaining_vectors: i64 = connection
            .query_row(
                "SELECT count(*) FROM project_document_embedding WHERE project_id=$pid;",
                params![context.project_id],
                |row| row.get(0),
            )
            .unwrap();
        let remaining_tasks: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='EMBED_PROJECT_DOCUMENT';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let remaining_fts: i64 = connection
            .query_row(
                "SELECT count(*) FROM project_document_fts WHERE project_id=$pid;",
                params![context.project_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining_vectors, 0);
        assert_eq!(remaining_tasks, 0);
        assert_eq!(remaining_fts, 5, "关闭远程向量不得删除本地全文索引");
    }

    #[test]
    fn handoff_watcher_syncs_stable_external_modification() {
        let context = context();
        context
            .service
            .create_drafts(&workspace_path(&context), &five_documents(), None)
            .unwrap();
        approve_all(&context);
        context.service.promote_drafts(&context.project_id).unwrap();
        context
            .service
            .handoff(&handoff_request(&workspace_path(&context)))
            .unwrap();
        let path = context.workspace.path().join(".memstack/03_CURRENT_STATUS.md");
        std::fs::write(
            path,
            render_document(ProjectDocumentType::CurrentStatus, "# 当前状态\n\n监听自动同步。"),
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let document = context
                .service
                .get_document(&context.project_id, ProjectDocumentType::CurrentStatus)
                .unwrap();
            if document.version == 2 && document.content.contains("监听自动同步") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "监听器未在超时前同步稳定内容");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    fn handoff_request(workspace: &str) -> ProjectHandoffRequest {
        ProjectHandoffRequest {
            workspace_path: workspace.to_string(),
            context_max_chars: 1000,
            active_decision_max_count: 5,
            current_status_max_chars: 1000,
            active_problem_max_count: 5,
            resolved_problem_max_count: 3,
            recent_changelog_max_count: 5,
            related_conclusion_card_max_count: 3,
            conclusion_card_max_chars: 500,
        }
    }

    #[test]
    fn section_and_entry_helpers() {
        let body = "# 标题\n\n## 小节A\n\n内容A1\n\n### 条目1\n\n内容1\n\n### 条目2\n\n内容2\n\n## 小节B\n\n内容B";
        let section = extract_section(body, "小节A").unwrap();
        assert!(section.contains("条目1"));
        assert!(!section.contains("内容B"));
        assert!(extract_section(body, "不存在").is_none());
        // 前 2 个条目：小节引言 + 第一个条目。
        let entries = first_entries(&section, 2);
        assert!(entries.contains("条目1"));
        assert!(!entries.contains("条目2"));
        // Changelog 的 h2 日期小节条目。
        let changelog = "# 变更记录\n\n## 2026-08-21\n\n完成 A。\n\n## 2026-08-20\n\n完成 B。";
        let limited = first_h2_entries(changelog, 1);
        assert!(limited.contains("2026-08-21"));
        assert!(!limited.contains("2026-08-20"));
        // PROB 编号收集。
        let problems = "### PROB-20260821-001 启动失败\n\n描述\n- 关联 PROB-20260820-002";
        assert!(collect_problem_ids(problems).contains(&"PROB-20260821-001".to_string()));
    }
}
