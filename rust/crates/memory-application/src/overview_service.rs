//! 总览服务：移植 C# `OverviewService`。
//!
//! 聚合查询 + 最近记忆 5 条 + 活动项目前 4 个 + 检索模式（Embedding 设置只读访问器）。
//!
//! 与 C# 的两处架构性差异（stdio 无本地端口）：
//! - `search_mode`：语义一致（enabled && configured → HYBRID / KEYWORD），
//!   但 `Configured` 仅判断 `embedding.api_key` 非空，不解密（最小只读访问器）。
//! - `mcp_status`：C# 读 10212 HTTP host 状态；Rust stdio 架构无常驻主机，
//!   改为「存在有效（未吊销）Token → READY，否则 FAILED」，报告记录偏差。

use std::sync::Arc;

use memory_domain::{BusinessError, McpActivitySummary, MemoryItem, OverviewClient, OverviewResult};
use rusqlite::{Connection, params};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::memory_service::{SELECT_MEMORY_SQL, read_memory};
use crate::project_service::ProjectService;
use crate::sqlite_errors::map_sqlite_error;

/// 使用固定数量查询返回总览聚合数据。
pub struct OverviewService {
    database: Database,
    projects: Arc<ProjectService>,
    clock: Arc<dyn Clock>,
    #[allow(dead_code)]
    ids: Arc<dyn IdGenerator>,
}

impl OverviewService {
    /// 创建总览服务。
    pub fn new(
        database: Database,
        projects: Arc<ProjectService>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            database,
            projects,
            clock,
            ids,
        }
    }

    /// 返回总览所需的全部真实数据。
    pub fn get(&self) -> Result<OverviewResult, BusinessError> {
        let connection = self.database.open()?;
        let (memories, projects, candidates, recent_assistant_name, last_mcp_call_at): (
            i64,
            i64,
            i64,
            Option<String>,
            Option<String>,
        ) = connection
            .query_row(
                "SELECT \
                    (SELECT count(*) FROM memory WHERE status='Active'), \
                    (SELECT count(*) FROM project WHERE is_archived=0), \
                    (SELECT count(*) FROM memory_candidate WHERE status='PENDING'), \
                    (SELECT display_name FROM mcp_token WHERE last_used_at IS NOT NULL ORDER BY last_used_at DESC LIMIT 1), \
                    (SELECT last_used_at FROM mcp_token WHERE last_used_at IS NOT NULL ORDER BY last_used_at DESC LIMIT 1);",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .map_err(map_sqlite_error)?;
        let recent = recent_memories_excluding_archived_projects(&connection)?;
        let active_projects: Vec<_> = self.projects.list(false)?.into_iter().take(4).collect();
        let search_mode = if embedding_enabled_and_configured(&connection) {
            "HYBRID"
        } else {
            "KEYWORD"
        };
        let mcp_status = if has_active_token(&connection, &format_storage_time(self.clock.now_utc())) {
            "READY"
        } else {
            "FAILED"
        };
        let now_text = format_storage_time(self.clock.now_utc());
        let mcp_activity = latest_mcp_activity(&connection)?;
        let (recent_clients, active_client_count) = recent_mcp_clients(&connection, &now_text)?;
        Ok(OverviewResult {
            memory_count: memories,
            project_count: projects,
            candidate_count: candidates,
            recent_memories: recent,
            active_projects,
            search_mode: search_mode.to_string(),
            mcp_status: mcp_status.to_string(),
            recent_assistant_name,
            last_mcp_call_at,
            mcp_activity,
            recent_clients,
            active_client_count,
        })
    }
}

/// 最近 5 条 Active 记忆（置顶优先，与记忆列表同排序），排除已归档项目的记忆。
///
/// 与 C# 基线的差异（用户验收反馈）：C# 直接复用记忆列表查询，归档项目的
/// 记忆仍会出现在总览「最近记忆」；此处按产品语义过滤——项目归档后其记忆
/// 不再进入总览，记忆本身不删除（项目恢复后重新可见）。
fn recent_memories_excluding_archived_projects(connection: &Connection) -> Result<Vec<MemoryItem>, BusinessError> {
    let sql = format!(
        "{SELECT_MEMORY_SQL} \
         WHERE m.status='Active' \
           AND (m.project_id IS NULL \
                OR m.project_id IN (SELECT id FROM project WHERE is_archived=0)) \
         ORDER BY m.is_pinned DESC, m.updated_at DESC, m.id DESC \
         LIMIT 5;"
    );
    let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
    let rows = statement.query_map([], read_memory).map_err(map_sqlite_error)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row.map_err(map_sqlite_error)?);
    }
    Ok(items)
}

/// 最近一次 MCP 记忆活动，无活动或字段为空时返回 `None`。
fn latest_mcp_activity(connection: &Connection) -> Result<Option<McpActivitySummary>, BusinessError> {
    let activity = connection
        .query_row(
            "SELECT display_name,last_memory_action,last_action_scope,last_action_at FROM mcp_token \
             WHERE last_action_at IS NOT NULL AND last_memory_action IS NOT NULL \
             ORDER BY last_action_at DESC LIMIT 1;",
            [],
            |row| {
                Ok(McpActivitySummary {
                    display_name: row.get(0)?,
                    action: row.get(1)?,
                    scope: row.get(2)?,
                    occurred_at: row.get(3)?,
                })
            },
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(map_sqlite_error)?;
    Ok(activity)
}

/// 未吊销、未过期的 AI 工具：按最近使用排序取前 6 个，并返回总数（供「+N」）。
fn recent_mcp_clients(connection: &Connection, now_text: &str) -> Result<(Vec<OverviewClient>, i64), BusinessError> {
    let mut statement = connection
        .prepare(
            "SELECT t.display_name, \
                    coalesce(s.transport,'stdio'), \
                    t.last_used_at \
             FROM mcp_token t \
             LEFT JOIN mcp_client_session s ON s.id=t.session_id \
             WHERE t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at > $now) \
             ORDER BY (t.last_used_at IS NULL), t.last_used_at DESC, t.created_at DESC;",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params![now_text], |row| {
            Ok(OverviewClient {
                display_name: row.get(0)?,
                transport: row.get(1)?,
                last_used_at: row.get(2)?,
            })
        })
        .map_err(map_sqlite_error)?;
    let mut clients = Vec::new();
    for row in rows {
        clients.push(row.map_err(map_sqlite_error)?);
    }
    let total = clients.len() as i64;
    clients.truncate(6);
    Ok((clients, total))
}

/// Embedding 检索模式判定：`embedding.enabled` 为真且 `embedding.api_key` 非空。
fn embedding_enabled_and_configured(connection: &Connection) -> bool {
    let enabled: Option<String> = connection
        .query_row(
            "SELECT setting_value FROM app_setting WHERE setting_key='embedding.enabled';",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .unwrap_or(None);
    let enabled = matches!(enabled.as_deref(), Some(value) if value.eq_ignore_ascii_case("true"));
    if !enabled {
        return false;
    }
    let api_key: Option<String> = connection
        .query_row(
            "SELECT setting_value FROM app_setting WHERE setting_key='embedding.api_key';",
            [],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .unwrap_or(None);
    api_key.is_some_and(|value| !value.is_empty())
}

/// 是否存在有效（未吊销且未过期）Token —— stdio 架构下的 MCP 可用性摘要。
fn has_active_token(connection: &Connection, now_text: &str) -> bool {
    connection
        .query_row(
            "SELECT count(*) FROM mcp_token \
             WHERE revoked_at IS NULL \
               AND (expires_at IS NULL OR expires_at > $now);",
            params![now_text],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use crate::mcp_access::McpAccessService;
    use crate::memory_service::MemoryService;
    use crate::project_service::ProjectService;
    use chrono::{TimeZone, Utc};
    use memory_domain::{McpAssistantType, McpPermission, MemoryScope, SaveMemoryRequest, SaveProjectRequest};

    struct TestContext {
        overview: OverviewService,
        memories: Arc<MemoryService>,
        projects: Arc<ProjectService>,
        access: Arc<McpAccessService>,
        database: Database,
        _temp: tempfile::TempDir,
    }

    fn context() -> TestContext {
        let temp = tempfile::tempdir().unwrap();
        drop(memory_storage::open_initialized(&temp.path().join("test.db")).unwrap());
        let database = Database::new(temp.path().join("test.db"));
        let clock = Arc::new(FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap()));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=20)
                .map(|index| format!("{index:08}-{index:04}-4{index:03}-8{index:03}-{index:012}"))
                .collect(),
        ));
        let memories = Arc::new(MemoryService::new(database.clone(), clock.clone(), ids.clone()));
        let projects = Arc::new(ProjectService::new(database.clone(), clock.clone(), ids.clone()));
        let access = Arc::new(McpAccessService::new(database.clone(), clock.clone(), ids));
        let overview = OverviewService::new(database.clone(), projects.clone(), clock, ids_for_overview());
        TestContext {
            overview,
            memories,
            projects,
            access,
            database,
            _temp: temp,
        }
    }

    fn ids_for_overview() -> Arc<dyn IdGenerator> {
        Arc::new(FixedIdGenerator::new(vec![
            "00000000-0000-4000-8000-000000000000".to_string(),
        ]))
    }

    fn save_request(content: &str) -> SaveMemoryRequest {
        SaveMemoryRequest {
            scope: memory_domain::MemoryScope::Personal,
            project_id: None,
            title: format!("标题-{content}"),
            summary: String::new(),
            content: content.to_string(),
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
    fn overview_aggregates_counts_and_recent_items() {
        let context = context();
        for index in 0..6 {
            context.memories.create(&save_request(&format!("内容{index}"))).unwrap();
        }
        let overview = context.overview.get().unwrap();
        assert_eq!(overview.memory_count, 6);
        assert_eq!(overview.recent_memories.len(), 5, "最近记忆最多 5 条");
        assert_eq!(overview.candidate_count, 0);
        assert_eq!(overview.search_mode, "KEYWORD");
        // 无有效 Token：FAILED。
        assert_eq!(overview.mcp_status, "FAILED");
        assert_eq!(overview.recent_assistant_name, None);
    }

    #[test]
    fn recent_memories_exclude_archived_project_memories() {
        let context = context();
        let project = context
            .projects
            .create(&SaveProjectRequest {
                name: "项目A".to_string(),
                description: String::new(),
                color: "#4f8cff".to_string(),
            })
            .unwrap();
        let mut project_memory = save_request("项目记忆");
        project_memory.scope = MemoryScope::Project;
        project_memory.project_id = Some(project.id.clone());
        context.memories.create(&project_memory).unwrap();
        context.memories.create(&save_request("个人记忆")).unwrap();

        // 归档前：两条记忆都出现在最近记忆。
        let overview = context.overview.get().unwrap();
        assert_eq!(overview.recent_memories.len(), 2);

        // 归档项目后：项目记忆同步归档，从最近记忆消失，个人记忆保留。
        context.projects.archive(&project.id).unwrap();
        let overview = context.overview.get().unwrap();
        let titles: Vec<&str> = overview
            .recent_memories
            .iter()
            .map(|item| item.title.as_str())
            .collect();
        assert_eq!(titles, vec!["标题-个人记忆"]);
        // 记忆同步归档（未删除）：Active 口径只剩个人 1 条，可在「已归档」按项目恢复。
        assert_eq!(overview.memory_count, 1);

        // 恢复项目：不反向恢复记忆（项目记忆仍在已归档，需手动恢复）。
        context.projects.restore(&project.id).unwrap();
        let overview = context.overview.get().unwrap();
        assert_eq!(overview.recent_memories.len(), 1);
    }

    #[test]
    fn mcp_status_follows_active_token_usage() {
        let context = context();
        let secret = context
            .access
            .create_token(&memory_domain::CreateMcpTokenRequest {
                assistant_type: McpAssistantType::Codex,
                display_name: "Codex".to_string(),
                permission: McpPermission::ReadWrite,
                project_id: None,
                expires_at: None,
            })
            .unwrap();
        let overview = context.overview.get().unwrap();
        assert_eq!(overview.mcp_status, "READY");
        // 未使用过：最近助手为空。
        assert_eq!(overview.recent_assistant_name, None);
        // 使用后：最近助手与最近调用时间出现。
        context.access.authenticate(&secret.plain_token).unwrap().unwrap();
        let overview = context.overview.get().unwrap();
        assert_eq!(overview.recent_assistant_name.as_deref(), Some("Codex"));
        assert!(overview.last_mcp_call_at.is_some());
        // 吊销后回到 FAILED。
        context.access.revoke(&secret.token.id).unwrap();
        let overview = context.overview.get().unwrap();
        assert_eq!(overview.mcp_status, "FAILED");
    }

    #[test]
    fn search_mode_follows_embedding_settings() {
        let context = context();
        // 未配置：KEYWORD。
        assert_eq!(context.overview.get().unwrap().search_mode, "KEYWORD");
        // 仅 enabled 无 key：仍 KEYWORD。
        let connection = context.database.open().unwrap();
        connection
            .execute(
                "INSERT INTO app_setting(setting_key,setting_value,updated_at) \
                 VALUES('embedding.enabled','true','2026-08-15T08:00:00.0000000+00:00');",
                [],
            )
            .unwrap();
        assert_eq!(context.overview.get().unwrap().search_mode, "KEYWORD");
        // enabled + api_key：HYBRID。
        connection
            .execute(
                "INSERT INTO app_setting(setting_key,setting_value,updated_at) \
                 VALUES('embedding.api_key','encrypted-blob','2026-08-15T08:00:00.0000000+00:00');",
                [],
            )
            .unwrap();
        assert_eq!(context.overview.get().unwrap().search_mode, "HYBRID");
    }
}
