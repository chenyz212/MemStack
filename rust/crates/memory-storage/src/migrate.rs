//! schema v1→v7 迁移链（SQL 与 C# `MemoryDatabase` 各段常量逐字一致）。
//!
//! 每段迁移在独立事务内执行：段 SQL + `PRAGMA user_version = N` 原子提交；
//! 任一步失败整体回滚且版本号不落库——与 C# `ExecuteMigrationAsync` 语义一致。

use memory_domain::{BusinessError, ErrorCode};
use rusqlite::Connection;

/// v1：基础十一表 + FTS5 虚表（对应 C# `SchemaSql`）。
const SCHEMA_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS app_setting (
    setting_key TEXT PRIMARY KEY,
    setting_value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS project (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL COLLATE NOCASE UNIQUE,
    description TEXT NOT NULL,
    color TEXT NOT NULL,
    is_archived INTEGER NOT NULL DEFAULT 0 CHECK (is_archived IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL CHECK (scope IN ('Personal', 'Project')),
    project_id TEXT NULL REFERENCES project(id),
    title TEXT NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    summary TEXT NOT NULL,
    content TEXT NOT NULL CHECK (length(content) BETWEEN 1 AND 200000),
    memory_type TEXT NOT NULL,
    keywords_json TEXT NOT NULL,
    tags_json TEXT NOT NULL,
    importance INTEGER NOT NULL CHECK (importance BETWEEN 1 AND 5),
    is_favorite INTEGER NOT NULL CHECK (is_favorite IN (0, 1)),
    is_pinned INTEGER NOT NULL CHECK (is_pinned IN (0, 1)),
    cloud_processing_allowed INTEGER NOT NULL CHECK (cloud_processing_allowed IN (0, 1)),
    status TEXT NOT NULL CHECK (status IN ('Active', 'Archived')),
    version INTEGER NOT NULL,
    content_checksum TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived_at TEXT NULL,
    CHECK ((scope = 'Personal' AND project_id IS NULL) OR (scope = 'Project' AND project_id IS NOT NULL)),
    UNIQUE (scope, project_id, content_checksum)
);
CREATE INDEX IF NOT EXISTS idx_memory_status_updated ON memory(status, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_memory_project_status ON memory(project_id, status, updated_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_content_unique ON memory(scope, ifnull(project_id, ''), content_checksum);
CREATE TABLE IF NOT EXISTS memory_revision (
    id TEXT PRIMARY KEY,
    memory_id TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    snapshot_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(memory_id, version)
);
CREATE TABLE IF NOT EXISTS memory_embedding (
    memory_id TEXT PRIMARY KEY REFERENCES memory(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    content_checksum TEXT NOT NULL,
    vector_blob BLOB NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_edge (
    memory_id_a TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
    memory_id_b TEXT NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
    semantic_score REAL NOT NULL,
    keyword_score REAL NOT NULL,
    project_boost REAL NOT NULL,
    combined_score REAL NOT NULL,
    dominant_signal TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(memory_id_a, memory_id_b),
    CHECK(memory_id_a < memory_id_b)
);
CREATE TABLE IF NOT EXISTS memory_candidate (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    project_id TEXT NULL REFERENCES project(id),
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    content TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_token (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    access_mode TEXT NOT NULL,
    project_scope_json TEXT NOT NULL,
    expires_at TEXT NULL,
    revoked_at TEXT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS background_task (
    id TEXT PRIMARY KEY,
    task_type TEXT NOT NULL,
    target_id TEXT NULL,
    status TEXT NOT NULL,
    attempt_count INTEGER NOT NULL,
    next_attempt_at TEXT NULL,
    error_code TEXT NULL,
    error_message TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    memory_id UNINDEXED,
    title_tokens,
    keyword_tokens,
    summary_tokens,
    content_tokens,
    tokenize = 'unicode61 remove_diacritics 2'
);
"#;

/// v2：项目工作空间绑定列（对应 C# `WorkspaceMigrationSql`）。
const SCHEMA_V2_SQL: &str = r#"
ALTER TABLE project ADD COLUMN workspace_key TEXT NULL;
ALTER TABLE project ADD COLUMN workspace_uri TEXT NULL;
ALTER TABLE project ADD COLUMN workspace_label TEXT NULL;
ALTER TABLE project ADD COLUMN workspace_last_seen_at TEXT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_project_workspace_key ON project(workspace_key) WHERE workspace_key IS NOT NULL;
"#;

/// v3：工作空间标识去重重建（对应 C# `WorkspaceIdentifierMigrationSql`）。
const SCHEMA_V3_SQL: &str = r#"
DROP INDEX IF EXISTS idx_project_workspace_key;
UPDATE project
SET workspace_key=NULL, workspace_uri=NULL, workspace_label=NULL, workspace_last_seen_at=NULL
WHERE id IN (
    SELECT id FROM (
        SELECT id, row_number() OVER (
            PARTITION BY lower(trim(workspace_label))
            ORDER BY workspace_last_seen_at DESC, updated_at DESC, id DESC
        ) AS binding_order
        FROM project
        WHERE workspace_label IS NOT NULL AND trim(workspace_label) <> ''
    ) WHERE binding_order > 1
);
UPDATE project
SET workspace_key=upper(trim(workspace_label)), workspace_uri=NULL
WHERE workspace_label IS NOT NULL AND trim(workspace_label) <> '';
CREATE UNIQUE INDEX IF NOT EXISTS idx_project_workspace_key ON project(workspace_key) WHERE workspace_key IS NOT NULL;
"#;

/// v4：历史版本收敛为「每条记忆至多一条上一版」（对应 C# `PreviousRevisionMigrationSql`）。
const SCHEMA_V4_SQL: &str = r#"
DELETE FROM memory_revision
WHERE id NOT IN (
    SELECT revision.id
    FROM memory_revision revision
    INNER JOIN memory current_memory ON current_memory.id=revision.memory_id
    WHERE revision.version=(
        SELECT max(candidate.version)
        FROM memory_revision candidate
        WHERE candidate.memory_id=revision.memory_id
          AND candidate.version < current_memory.version
    )
);
"#;

/// v5：来源列 / Token 扩展列 / 候选扩展列（对应 C# `McpAndCandidateMigrationSql`）。
const SCHEMA_V5_SQL: &str = r#"
ALTER TABLE memory ADD COLUMN created_source TEXT NOT NULL DEFAULT '桌面客户端';
ALTER TABLE memory ADD COLUMN updated_source TEXT NOT NULL DEFAULT '桌面客户端';
ALTER TABLE project ADD COLUMN aliases_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE project ADD COLUMN version INTEGER NOT NULL DEFAULT 1;

ALTER TABLE mcp_token ADD COLUMN assistant_type TEXT NOT NULL DEFAULT 'Generic';
ALTER TABLE mcp_token ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
ALTER TABLE mcp_token ADD COLUMN token_prefix TEXT NOT NULL DEFAULT '';
ALTER TABLE mcp_token ADD COLUMN token_ciphertext TEXT NOT NULL DEFAULT '';
ALTER TABLE mcp_token ADD COLUMN permission TEXT NOT NULL DEFAULT 'Read';
ALTER TABLE mcp_token ADD COLUMN project_id TEXT NULL REFERENCES project(id);
ALTER TABLE mcp_token ADD COLUMN last_used_at TEXT NULL;
UPDATE mcp_token SET display_name=name WHERE display_name='';
UPDATE mcp_token SET permission=CASE WHEN access_mode='ReadWrite' THEN 'ReadWrite' ELSE 'Read' END;
CREATE INDEX IF NOT EXISTS idx_mcp_token_active ON mcp_token(revoked_at, expires_at, created_at DESC);

ALTER TABLE memory_candidate ADD COLUMN memory_type TEXT NOT NULL DEFAULT 'NOTE';
ALTER TABLE memory_candidate ADD COLUMN keywords_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE memory_candidate ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE memory_candidate ADD COLUMN importance INTEGER NOT NULL DEFAULT 3;
ALTER TABLE memory_candidate ADD COLUMN cloud_processing_allowed INTEGER NOT NULL DEFAULT 1;
ALTER TABLE memory_candidate ADD COLUMN content_checksum TEXT NOT NULL DEFAULT '';
ALTER TABLE memory_candidate ADD COLUMN source_name TEXT NOT NULL DEFAULT '旧版候选';
ALTER TABLE memory_candidate ADD COLUMN version INTEGER NOT NULL DEFAULT 1;
DELETE FROM memory_candidate
WHERE id NOT IN (
    SELECT max(id) FROM memory_candidate
    WHERE status='PENDING'
    GROUP BY scope,ifnull(project_id,''),content_checksum
) AND status='PENDING';
CREATE UNIQUE INDEX IF NOT EXISTS idx_candidate_content_unique
ON memory_candidate(scope,ifnull(project_id,''),content_checksum) WHERE status='PENDING';
CREATE INDEX IF NOT EXISTS idx_candidate_updated ON memory_candidate(updated_at DESC, id DESC);
"#;

/// v6：动态 MCP 客户端会话表 + Token 关联（对应 C# `McpClientSessionMigrationSql`）。
const SCHEMA_V6_SQL: &str = r#"
-- 动态 MCP 客户端会话表：一个用户创建的 AI 工具对应一行。
CREATE TABLE IF NOT EXISTS mcp_client_session (
    id TEXT PRIMARY KEY,
    client_key TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    client_version TEXT NULL,
    transport TEXT NOT NULL DEFAULT 'http',
    last_seen_at TEXT NULL,
    call_count INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mcp_client_session_created
ON mcp_client_session(created_at DESC, id DESC);

-- 已有 mcp_token 关联到自动创建的客户端会话；无法推导名称时使用 display_name 兜底，禁止丢失。
ALTER TABLE mcp_token ADD COLUMN session_id TEXT NULL REFERENCES mcp_client_session(id) ON DELETE SET NULL;

-- 为每个已有 Token 自动创建对应客户端会话；同名 Token 共享同一会话（client_key 唯一约束兜底）。
INSERT INTO mcp_client_session(id, client_key, display_name, client_version, transport, last_seen_at, call_count, created_at, updated_at)
SELECT
    lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(2))) || '-' || lower(hex(randomblob(6))),
    lower(replace(trim(display_name), ' ', '')),
    trim(display_name),
    NULL,
    'http',
    last_used_at,
    0,
    coalesce(created_at, '1970-01-01T00:00:00+00:00'),
    coalesce(created_at, '1970-01-01T00:00:00+00:00')
FROM mcp_token
WHERE trim(display_name) <> ''
GROUP BY lower(replace(trim(display_name), ' ', ''))
ON CONFLICT(client_key) DO NOTHING;

-- 把每个 Token 关联到对应会话。
UPDATE mcp_token
SET session_id = (
    SELECT s.id FROM mcp_client_session s
    WHERE s.client_key = lower(replace(trim(mcp_token.display_name), ' ', ''))
)
WHERE session_id IS NULL
  AND EXISTS (
    SELECT 1 FROM mcp_client_session s
    WHERE s.client_key = lower(replace(trim(mcp_token.display_name), ' ', ''))
  );

-- 一个会话至多一个有效令牌（NULL session_id 不限，用于尚未关联的旧 Token）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_mcp_token_session_active
ON mcp_token(session_id) WHERE revoked_at IS NULL AND session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_mcp_token_session ON mcp_token(session_id);
"#;

/// v7：无连字符 session ID 修复为标准 GUID（对应 C# `FixSessionIdFormatMigrationSql`）。
const SCHEMA_V7_SQL: &str = r#"
-- 延迟外键检查到事务提交，避免更新主键时子表外键约束失败。
PRAGMA defer_foreign_keys = ON;

-- 把 mcp_client_session 中 32 字符无连字符的 id 转为标准 GUID 格式（带连字符）。
UPDATE mcp_client_session
SET id = substr(id,1,8) || '-' || substr(id,9,4) || '-' || substr(id,13,4) || '-' || substr(id,17,4) || '-' || substr(id,21,12)
WHERE length(id) = 32 AND instr(id, '-') = 0;

-- 同步修正 mcp_token 中的 session_id 外键。
UPDATE mcp_token
SET session_id = substr(session_id,1,8) || '-' || substr(session_id,9,4) || '-' || substr(session_id,13,4) || '-' || substr(session_id,17,4) || '-' || substr(session_id,21,12)
WHERE session_id IS NOT NULL AND length(session_id) = 32 AND instr(session_id, '-') = 0;
"#;

/// v8：mcp_token 增加最近记忆活动字段（总览活动文案数据源）。
const SCHEMA_V8_SQL: &str = r#"
-- 最近一次成功的 MCP 记忆活动。
ALTER TABLE mcp_token ADD COLUMN last_memory_action TEXT NULL;
-- Personal / Project / Mixed。
ALTER TABLE mcp_token ADD COLUMN last_action_scope TEXT NULL;
ALTER TABLE mcp_token ADD COLUMN last_action_at TEXT NULL;
"#;

/// v9：项目全局文档 + 初始化草稿 + 晋升记录 + 结论卡片扩展表。
///
/// - 正式 Markdown 文件是唯一事实来源，`project_document` 只保存镜像与同步状态。
/// - `project_document_draft` 保存初始化草稿与审核状态（每项目每类型唯一）。
/// - `project_document_promotion` 记录晋升操作进度，用于崩溃恢复。
/// - `conclusion_card` 是 SOLUTION 记忆的结构化扩展（候选数据存 memory_candidate）。
/// - project 表新增文档 Embedding 开关（默认关闭）与最近工作空间绝对路径。
const SCHEMA_V9_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS project_document (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES project(id),
    document_type TEXT NOT NULL CHECK (document_type IN ('CONTEXT','DECISIONS','CURRENT_STATUS','PROBLEMS','CHANGELOG')),
    relative_path TEXT NOT NULL,
    content TEXT NOT NULL,
    checksum TEXT NOT NULL,
    version INTEGER NOT NULL,
    previous_content TEXT NULL,
    previous_checksum TEXT NULL,
    previous_version INTEGER NULL,
    embedding_enabled INTEGER NOT NULL DEFAULT 0 CHECK (embedding_enabled IN (0, 1)),
    sync_status TEXT NOT NULL DEFAULT 'SYNCED' CHECK (sync_status IN ('SYNCED','SYNC_PENDING','CONFLICT','FORMAT_ERROR','REPAIR_PENDING')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, document_type)
);
CREATE INDEX IF NOT EXISTS idx_project_document_updated
ON project_document(updated_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS project_document_draft (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES project(id),
    document_type TEXT NOT NULL CHECK (document_type IN ('CONTEXT','DECISIONS','CURRENT_STATUS','PROBLEMS','CHANGELOG')),
    relative_path TEXT NOT NULL,
    content TEXT NOT NULL,
    checksum TEXT NOT NULL,
    version INTEGER NOT NULL,
    review_status TEXT NOT NULL DEFAULT 'PENDING_REVIEW' CHECK (review_status IN ('PENDING_REVIEW','APPROVED')),
    approved_version INTEGER NULL,
    last_change_reason TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(project_id, document_type)
);
CREATE INDEX IF NOT EXISTS idx_project_document_draft_updated
ON project_document_draft(updated_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS project_document_promotion (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES project(id),
    status TEXT NOT NULL CHECK (status IN ('IN_PROGRESS','COMPLETED','FAILED')),
    error_message TEXT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_project_document_promotion_active
ON project_document_promotion(project_id) WHERE status='IN_PROGRESS';

CREATE TABLE IF NOT EXISTS conclusion_card (
    memory_id TEXT PRIMARY KEY REFERENCES memory(id) ON DELETE CASCADE,
    problem_id TEXT NULL,
    structured_payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_conclusion_card_problem ON conclusion_card(problem_id);

-- 结论卡片候选：结构化数据随候选行保存，确认时在同一事务内转为 conclusion_card。
ALTER TABLE memory_candidate ADD COLUMN structured_payload_json TEXT NULL;

ALTER TABLE project ADD COLUMN project_document_embedding_enabled INTEGER NOT NULL DEFAULT 0
    CHECK (project_document_embedding_enabled IN (0, 1));
ALTER TABLE project ADD COLUMN project_document_workspace_path TEXT NULL;
"#;

/// v10：项目文档独立全文索引与向量索引。
///
/// 项目文档不是普通记忆，因此使用独立表，避免污染 memory 与 memory_embedding 契约。
const SCHEMA_V10_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS project_document_fts USING fts5(
    project_id UNINDEXED,
    document_type UNINDEXED,
    content_tokens,
    tokenize = 'unicode61 remove_diacritics 2'
);
INSERT INTO project_document_fts(project_id,document_type,content_tokens)
SELECT project_id,document_type,content FROM project_document;

CREATE TABLE IF NOT EXISTS project_document_embedding (
    project_id TEXT NOT NULL,
    document_type TEXT NOT NULL CHECK (document_type IN ('CONTEXT','DECISIONS','CURRENT_STATUS','PROBLEMS','CHANGELOG')),
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    content_checksum TEXT NOT NULL,
    vector_blob BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(project_id, document_type),
    FOREIGN KEY(project_id, document_type) REFERENCES project_document(project_id, document_type) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_project_document_embedding_updated
ON project_document_embedding(updated_at DESC, project_id, document_type);
"#;

/// 完整迁移链（版本 → 段 SQL）。
const MIGRATION_CHAIN: [(i64, &str); 10] = [
    (1, SCHEMA_V1_SQL),
    (2, SCHEMA_V2_SQL),
    (3, SCHEMA_V3_SQL),
    (4, SCHEMA_V4_SQL),
    (5, SCHEMA_V5_SQL),
    (6, SCHEMA_V6_SQL),
    (7, SCHEMA_V7_SQL),
    (8, SCHEMA_V8_SQL),
    (9, SCHEMA_V9_SQL),
    (10, SCHEMA_V10_SQL),
];

/// 迁移执行报告（供日志与验收记录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub from_version: i64,
    pub to_version: i64,
    pub steps: Vec<i64>,
}

/// 执行 v1→v7 迁移链：从当前 `user_version` 逐段升级到 [`SUPPORTED_SCHEMA_VERSION`]。
///
/// 已处于目标版本时返回空 steps 的报告（幂等）。
pub fn run_migrations(connection: &mut Connection) -> Result<MigrationReport, BusinessError> {
    let mut report = MigrationReport {
        from_version: crate::schema::detect_schema_version(connection)?,
        to_version: 0,
        steps: Vec::new(),
    };
    for (version, sql) in MIGRATION_CHAIN {
        if report.steps.is_empty() && version <= report.from_version {
            continue;
        }
        // 上一段失败即中断；成功则继续后续段落。
        migrate_step(connection, sql, version)?;
        report.steps.push(version);
    }
    report.to_version = report.steps.last().copied().unwrap_or(report.from_version);
    Ok(report)
}

/// 在事务内执行一段迁移并写入目标版本号（失败回滚，版本号不落库）。
fn migrate_step(connection: &mut Connection, sql: &str, target_version: i64) -> Result<(), BusinessError> {
    let internal = |error: rusqlite::Error| {
        BusinessError::with_message(
            ErrorCode::InternalError,
            format!("结构升级到 v{target_version} 失败：{error}"),
        )
    };
    let transaction = connection.transaction().map_err(internal)?;
    transaction.execute_batch(sql).map_err(internal)?;
    transaction
        .execute_batch(&format!("PRAGMA user_version = {target_version};"))
        .map_err(internal)?;
    transaction.commit().map_err(internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_migrates_to_version_10_with_all_tables() {
        let directory = std::env::temp_dir().join(format!("memstack-migrate-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("memory.db");
        let mut connection = crate::connection::open_connection(&path).unwrap();
        crate::connection::init_pragmas(&connection).unwrap();

        let report = run_migrations(&mut connection).unwrap();
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, 10);
        assert_eq!(report.steps, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

        assert_eq!(crate::schema::detect_schema_version(&connection).unwrap(), 10);
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type IN ('table','view') AND name IN
                    ('app_setting','project','memory','memory_revision','memory_embedding','memory_edge',
                     'memory_candidate','mcp_token','mcp_client_session','background_task','memory_fts',
                     'project_document','project_document_draft','project_document_promotion','conclusion_card',
                     'project_document_fts','project_document_embedding');",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 17);
        // v8：活动字段已就位。
        let action_columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('mcp_token') WHERE name IN
                    ('last_memory_action','last_action_scope','last_action_at');",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(action_columns, 3);
        // v9：项目文档 Embedding 开关与工作空间路径列已就位。
        let project_doc_columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('project') WHERE name IN
                    ('project_document_embedding_enabled','project_document_workspace_path');",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(project_doc_columns, 2);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn migrations_are_idempotent_at_target_version() {
        let directory = std::env::temp_dir().join(format!("memstack-migrate-idempotent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("memory.db");
        let mut connection = crate::connection::open_connection(&path).unwrap();
        run_migrations(&mut connection).unwrap();
        let report = run_migrations(&mut connection).unwrap();
        assert!(report.steps.is_empty());
        assert_eq!(report.to_version, 10);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// v9：project_document 唯一约束（project_id + document_type）生效。
    #[test]
    fn project_document_unique_constraint_rejects_duplicate_type() {
        let directory = std::env::temp_dir().join(format!("memstack-migrate-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("memory.db");
        let mut connection = crate::connection::open_connection(&path).unwrap();
        run_migrations(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO project(id,name,description,color,is_archived,created_at,updated_at)
                 VALUES('p1','项目','','#111111',0,'2026-08-21T00:00:00+00:00','2026-08-21T00:00:00+00:00');",
                [],
            )
            .unwrap();
        let insert = |id: &str| {
            connection.execute(
                "INSERT INTO project_document(id,project_id,document_type,relative_path,content,checksum,version,created_at,updated_at)
                 VALUES($id,'p1','CONTEXT','01_CONTEXT.md','内容','ck',1,'2026-08-21T00:00:00+00:00','2026-08-21T00:00:00+00:00');",
                [id],
            )
        };
        insert("d1").unwrap();
        assert!(insert("d2").is_err(), "同项目同类型必须唯一");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
