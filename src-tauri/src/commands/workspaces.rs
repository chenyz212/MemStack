//! 工作空间 + 总览 Commands（§5.5 / §5.1，共 3 个）。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{OverviewResult, WorkspaceMemoryRequest, WorkspaceMemoryResult, WorkspaceResolution};

/// 解析工作空间绑定状态。
#[tauri::command]
pub async fn resolve_workspace(
    state: State<'_, AppState>,
    workspace_identifier: String,
) -> Result<WorkspaceResolution, CommandError> {
    resolve_workspace_impl(&state, &workspace_identifier)
}

/// 通过工作空间写入记忆（未绑定时返回追问）。
#[tauri::command]
pub async fn store_workspace_memory(
    state: State<'_, AppState>,
    request: WorkspaceMemoryRequest,
) -> Result<WorkspaceMemoryResult, CommandError> {
    store_workspace_memory_impl(&state, &request)
}

/// 总览聚合数据。
#[tauri::command]
pub async fn get_overview(state: State<'_, AppState>) -> Result<OverviewResult, CommandError> {
    get_overview_impl(&state)
}

/// 数据库数据版本（`PRAGMA data_version`）：其他连接（MCP 进程）提交后递增。
/// 供前端 1 秒轻探测，变化时才执行真正的数据刷新（替代定时全量拉取）。
#[tauri::command]
pub async fn get_data_version(state: State<'_, AppState>) -> Result<i64, CommandError> {
    get_data_version_impl(&state)
}

pub fn resolve_workspace_impl(
    state: &AppState,
    workspace_identifier: &str,
) -> Result<WorkspaceResolution, CommandError> {
    Ok(state.workspaces.resolve(workspace_identifier)?)
}

pub fn store_workspace_memory_impl(
    state: &AppState,
    request: &WorkspaceMemoryRequest,
) -> Result<WorkspaceMemoryResult, CommandError> {
    Ok(state.workspaces.store_memory(request)?)
}

pub fn get_overview_impl(state: &AppState) -> Result<OverviewResult, CommandError> {
    Ok(state.overview.get()?)
}

pub fn get_data_version_impl(state: &AppState) -> Result<i64, CommandError> {
    let connection = state.data_version_probe.lock().expect("探测连接锁中毒");
    connection
        .query_row("PRAGMA data_version;", [], |row| row.get(0))
        .map_err(|error| {
            CommandError::with_message(
                memory_domain::ErrorCode::InternalError,
                format!("读取数据版本失败：{error}"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::SaveProjectRequest;

    fn state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        (AppState::build(database_path).unwrap(), temp)
    }

    #[test]
    fn resolve_unbound_workspace_asks_project_name() {
        let (state, _temp) = state();
        let resolution = resolve_workspace_impl(&state, "C:\\demo\\workspace").unwrap();
        assert_eq!(resolution.status, "PROJECT_NAME_REQUIRED");
        assert!(resolution.question.is_some());
    }

    #[test]
    fn data_version_advances_only_on_foreign_commit() {
        let (state, _temp) = state();

        // 基线；探测连接自身无写提交 → 版本不变。
        let baseline = get_data_version_impl(&state).unwrap();
        assert_eq!(get_data_version_impl(&state).unwrap(), baseline);

        // 服务层每操作开新连接（等价 MCP 进程的外部提交）→ 版本递增。
        state
            .memories
            .create(&memory_domain::SaveMemoryRequest {
                scope: memory_domain::MemoryScope::Personal,
                project_id: None,
                title: "数据版本探测".to_string(),
                summary: String::new(),
                content: "probe".to_string(),
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
        assert!(get_data_version_impl(&state).unwrap() > baseline);
    }

    #[test]
    fn workspace_flow_bind_then_store_memory() {
        let (state, _temp) = state();
        let project = state
            .projects
            .create(&SaveProjectRequest {
                name: "忆栈".to_string(),
                description: String::new(),
                color: "#4f8cff".to_string(),
            })
            .unwrap();
        state
            .workspaces
            .bind_workspace(&project.id, "C:\\demo\\workspace")
            .unwrap();

        let result = store_workspace_memory_impl(
            &state,
            &WorkspaceMemoryRequest {
                workspace_identifier: "C:\\demo\\workspace".to_string(),
                project_name: None,
                title: "标题".to_string(),
                summary: String::new(),
                content: "工作空间记忆".to_string(),
                memory_type: "NOTE".to_string(),
                keywords: vec![],
                tags: vec![],
                importance: 3,
                cloud_processing_allowed: false,
            },
        )
        .unwrap();
        assert_eq!(result.status, "STORED");
        let memory = result.memory.expect("应返回已存记忆");
        assert_eq!(memory.project_id.as_deref(), Some(project.id.as_str()));

        let overview = get_overview_impl(&state).unwrap();
        assert_eq!(overview.memory_count, 1);
        assert_eq!(overview.project_count, 1);
    }
}
