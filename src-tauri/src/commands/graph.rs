//! 记忆图谱 Commands（3 个）：全局圆形图谱 / 记忆邻域 / 全量关系重建。
//!
//! Command 层保持薄映射，关系计算全部在 `GraphService`。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{GraphGlobalQuery, GraphNeighborhoodQuery, GraphResult, RebuildTicket};

/// 获取全局圆形图谱（节点上限 120，后端强制 ≤ 300）。
#[tauri::command]
pub async fn get_global_graph(
    state: State<'_, AppState>,
    request: GraphGlobalQuery,
) -> Result<GraphResult, CommandError> {
    get_global_graph_impl(&state, &request)
}

/// 获取指定记忆为圆心的一跳/两跳邻域图谱。
#[tauri::command]
pub async fn get_neighborhood_graph(
    state: State<'_, AppState>,
    request: GraphNeighborhoodQuery,
) -> Result<GraphResult, CommandError> {
    get_neighborhood_graph_impl(&state, &request)
}

/// 排队执行全量关系重建（后台 Worker 消费，不阻塞界面）。
#[tauri::command]
pub async fn rebuild_graph(state: State<'_, AppState>) -> Result<RebuildTicket, CommandError> {
    rebuild_graph_impl(&state)
}

pub fn get_global_graph_impl(state: &AppState, request: &GraphGlobalQuery) -> Result<GraphResult, CommandError> {
    Ok(state.graph.global_graph(request)?)
}

pub fn get_neighborhood_graph_impl(
    state: &AppState,
    request: &GraphNeighborhoodQuery,
) -> Result<GraphResult, CommandError> {
    Ok(state.graph.neighborhood(request)?)
}

pub fn rebuild_graph_impl(state: &AppState) -> Result<RebuildTicket, CommandError> {
    Ok(state.graph.queue_full_rebuild()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use memory_domain::{MemoryScope, SaveMemoryRequest};

    fn state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        (AppState::build(database_path).unwrap(), temp)
    }

    fn request(title: &str, keywords: Vec<String>) -> SaveMemoryRequest {
        SaveMemoryRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title: title.to_string(),
            summary: String::new(),
            content: format!("{title} 正文"),
            memory_type: "NOTE".to_string(),
            keywords,
            tags: vec![],
            importance: 3,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: false,
            expected_version: None,
        }
    }

    #[test]
    fn global_and_neighborhood_return_real_edges() {
        let (state, _temp) = state();
        let first = state
            .memories
            .create(&request("图谱命令甲", vec!["共享".to_string()]))
            .unwrap();
        let second = state
            .memories
            .create(&request("图谱命令乙", vec!["共享".to_string()]))
            .unwrap();
        state.graph.recompute_all().unwrap();
        // 清理 create() 自动排队的单记忆任务，使构建状态回到 READY。
        {
            use memory_application::db::Database;
            Database::new(state.database_path.clone())
                .open()
                .unwrap()
                .execute(
                    "DELETE FROM background_task WHERE task_type IN ('REBUILD_GRAPH_MEMORY','REBUILD_GRAPH_ALL');",
                    [],
                )
                .unwrap();
        }

        let global = get_global_graph_impl(&state, &memory_domain::GraphGlobalQuery::default()).unwrap();
        assert_eq!(global.total_nodes, 2);
        assert_eq!(global.edges.len(), 1);
        assert_eq!(global.build_status, "READY");

        let neighborhood = get_neighborhood_graph_impl(
            &state,
            &GraphNeighborhoodQuery {
                memory_id: first.id.clone(),
                depth: 2,
                min_score: 0.3,
                ..GraphNeighborhoodQuery::default()
            },
        )
        .unwrap();
        assert_eq!(neighborhood.center_memory_id, Some(first.id.clone()));
        assert!(neighborhood.nodes.iter().any(|node| node.id == second.id));
    }

    #[test]
    fn rebuild_queues_background_task() {
        let (state, _temp) = state();
        let ticket = rebuild_graph_impl(&state).unwrap();
        assert_eq!(ticket.status, "PENDING");
        let global = get_global_graph_impl(&state, &memory_domain::GraphGlobalQuery::default()).unwrap();
        assert_eq!(global.build_status, "BUILDING");
    }
}
