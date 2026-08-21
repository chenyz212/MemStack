//! 桌面进程共享状态：装配全部 memory-application 服务（与 stdio main.rs 同装配）。
//!
//! 路径解析规则（§6.3）：`MEMSTACK_DB_PATH` env 优先 → `data_dir()` 生产规则
//! （`memory.db` 优先，回退 `desktop-memory.db`）。
//! 打开流程：`open_initialized`（迁移锁 + schema 7）→ `verify_fts5` → 各 Service::new。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use memory_application::candidate_service::MemoryCandidateService;
use memory_application::clock::SystemClock;
use memory_application::db::Database;
use memory_application::embedding_service::{EmbeddingService, UreqEmbeddingClient};
use memory_application::embedding_worker::{self, WorkerHandle};
use memory_application::graph_service::GraphService;
use memory_application::mcp_access::McpAccessService;
use memory_application::memory_service::MemoryService;
use memory_application::overview_service::OverviewService;
use memory_application::project_service::ProjectService;
use memory_application::search_service::{EmbeddingQueryVectors, SearchService};
use memory_application::workspace_service::WorkspaceService;
use memory_domain::{BusinessError, ErrorCode, McpCallerContext, McpPermission};

/// Tauri 托管状态：数据库路径 + 全部业务服务 + Embedding Worker 生命周期。
pub struct AppState {
    /// 已解析并完成初始化的数据库文件路径。
    pub database_path: PathBuf,
    /// 记忆服务。
    pub memories: Arc<MemoryService>,
    /// 项目服务。
    pub projects: Arc<ProjectService>,
    /// 工作空间服务。
    pub workspaces: Arc<WorkspaceService>,
    /// 候选记忆服务。
    pub candidates: Arc<MemoryCandidateService>,
    /// MCP 访问服务（客户端管理 + Token 三件套）。
    pub access: Arc<McpAccessService>,
    /// 总览服务。
    pub overview: Arc<OverviewService>,
    /// Embedding 设置服务。
    pub embedding: Arc<EmbeddingService>,
    /// 检索服务（FTS + RRF + MMR + 模糊回退 + 图谱一跳扩展）。
    pub search: Arc<SearchService>,
    /// 记忆图谱服务（memory_edge 关系计算与全局/邻域查询）。
    pub graph: Arc<GraphService>,
    /// 数据版本探测连接（专用长连接：`PRAGMA data_version` 仅在
    /// 其他连接提交时递增，供前端 1 秒轻探测替代定时全量刷新）。
    pub data_version_probe: Mutex<rusqlite::Connection>,
    /// Embedding Worker 句柄（启动后由 `shutdown_worker` 消费）。
    worker: Mutex<Option<WorkerHandle>>,
}

impl AppState {
    /// 装配全部服务：打开数据库（迁移 + FTS5 校验）并构造各 Service。
    pub fn build(database_path: PathBuf) -> Result<Self, BusinessError> {
        let connection = memory_storage::open_initialized(&database_path)?;
        memory_storage::verify_fts5(&connection)?;
        drop(connection);

        let database = Database::new(&database_path);
        let clock = Arc::new(SystemClock);
        let ids = Arc::new(memory_application::GuidGenerator);
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
        let access = Arc::new(McpAccessService::new(database.clone(), clock.clone(), ids.clone()));
        let overview = Arc::new(OverviewService::new(
            database.clone(),
            projects.clone(),
            clock.clone(),
            ids.clone(),
        ));
        let embedding = Arc::new(EmbeddingService::new(
            database.clone(),
            clock.clone(),
            ids.clone(),
            Arc::new(UreqEmbeddingClient),
        ));
        let search = Arc::new(SearchService::new(
            database.clone(),
            Arc::new(EmbeddingQueryVectors::new(embedding.clone())),
        ));
        let graph = Arc::new(GraphService::new(database, clock.clone(), ids));
        // 迁移在上方 open_initialized 已完成，探测连接只需普通打开。
        let data_version_probe = memory_storage::open_connection(&database_path)?;
        Ok(Self {
            database_path,
            memories,
            projects,
            workspaces,
            candidates,
            access,
            overview,
            embedding,
            search,
            graph,
            data_version_probe: Mutex::new(data_version_probe),
            worker: Mutex::new(None),
        })
    }

    /// 启动通用后台 Worker（Embedding + 图谱关系重算；桌面进程是唯一消费者）。
    pub fn spawn_worker(&self) {
        let mut worker = self.worker.lock().expect("embedding worker 锁中毒");
        if worker.is_none() {
            *worker = Some(embedding_worker::spawn_embedding_worker(
                Database::new(&self.database_path),
                self.embedding.clone(),
                self.graph.clone(),
                Arc::new(SystemClock),
            ));
            // 升级库自愈：存量记忆从未建过关系时排队一次全量重算（Worker 随即消费，
            // 图谱页经 buildStatus=BUILDING 轮询自动刷新）。失败不阻断启动。
            if let Err(error) = self.graph.queue_startup_rebuild_if_needed() {
                eprintln!("图谱启动自愈排队失败：{error:?}");
            }
        }
    }

    /// 停止 Embedding Worker 并等待线程退出（幂等；线程内 100ms 分片睡眠，退出迅速）。
    pub fn shutdown_worker(&self) {
        if let Some(handle) = self.worker.lock().expect("embedding worker 锁中毒").take() {
            handle.stop();
        }
    }

    /// 桌面固定调用方上下文（对齐 C# `ApiEndpoints`：Guid.Empty + 「桌面客户端」 + ReadWrite）。
    pub fn desktop_caller() -> McpCallerContext {
        McpCallerContext {
            token_id: "00000000-0000-0000-0000-000000000000".to_string(),
            display_name: "桌面客户端".to_string(),
            permission: McpPermission::ReadWrite,
            project_id: None,
        }
    }
}

/// 解析数据库路径：`MEMSTACK_DB_PATH` env 优先，否则生产数据目录规则。
pub fn resolve_database_path() -> Result<PathBuf, BusinessError> {
    if let Ok(explicit) = std::env::var("MEMSTACK_DB_PATH")
        && !explicit.trim().is_empty()
    {
        return Ok(PathBuf::from(explicit));
    }
    let data_dir = memory_platform::data_dir()
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("解析数据目录失败：{error}")))?;
    memory_storage::resolve_desktop_database_path(&data_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let state = AppState::build(database_path).unwrap();
        (state, temp)
    }

    #[test]
    fn app_state_builds_all_services_with_schema7() {
        let (state, temp) = temp_state();
        // schema 版本直接读库验证。
        let connection = Database::new(temp.path().join("test.db")).open().unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap_or(0);
        assert_eq!(version, 8, "schema 应为 8");
        // 各服务已装配（以一次真实调用验证非空可用）。
        assert!(state.memories.get_facets().is_ok());
        assert!(state.projects.list(false).is_ok());
        assert!(state.workspaces.resolve("C:\\demo\\workspace").is_ok());
        assert!(state.candidates.list(&AppState::desktop_caller()).is_ok());
        assert!(state.access.list_clients().is_ok());
        assert!(state.overview.get().is_ok());
        assert!(state.embedding.get_settings().is_ok());
        assert!(
            state
                .search
                .search(&memory_domain::SearchRequest {
                    query: "忆栈".to_string(),
                    scope: None,
                    project_id: None,
                    memory_type: None,
                    tag: None,
                    limit: 5,
                    semantic_enabled: false,
                })
                .is_ok()
        );
        state.shutdown_worker();
    }

    #[test]
    fn worker_spawn_and_shutdown_are_idempotent() {
        let (state, _temp) = temp_state();
        state.spawn_worker();
        // 重复启动不创建第二个线程。
        state.spawn_worker();
        state.shutdown_worker();
        // 幂等：再次停止不 panic。
        state.shutdown_worker();
    }

    #[test]
    fn desktop_caller_matches_csharp_baseline() {
        let caller = AppState::desktop_caller();
        assert_eq!(caller.token_id, "00000000-0000-0000-0000-000000000000");
        assert_eq!(caller.display_name, "桌面客户端");
        assert_eq!(caller.permission, McpPermission::ReadWrite);
        assert!(caller.project_id.is_none());
    }
}
