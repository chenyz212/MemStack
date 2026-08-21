//! 项目 Commands（§5.2，7 个 + 彻底删除）：列表 / 创建 / 更新 / 归档 / 恢复 / 绑定 / 解绑工作空间。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{DeletedProjectStats, ProjectItem, SaveProjectRequest};

/// 列出项目（含/不含已归档）。
#[tauri::command]
pub async fn list_projects(
    state: State<'_, AppState>,
    include_archived: bool,
) -> Result<Vec<ProjectItem>, CommandError> {
    list_projects_impl(&state, include_archived)
}

/// 创建项目。
#[tauri::command]
pub async fn create_project(
    state: State<'_, AppState>,
    request: SaveProjectRequest,
) -> Result<ProjectItem, CommandError> {
    create_project_impl(&state, request)
}

/// 更新项目。
#[tauri::command]
pub async fn update_project(
    state: State<'_, AppState>,
    id: String,
    request: SaveProjectRequest,
) -> Result<ProjectItem, CommandError> {
    update_project_impl(&state, &id, request)
}

/// 归档项目。
#[tauri::command]
pub async fn archive_project(state: State<'_, AppState>, id: String) -> Result<ProjectItem, CommandError> {
    archive_project_impl(&state, &id)
}

/// 恢复项目。
#[tauri::command]
pub async fn restore_project(state: State<'_, AppState>, id: String) -> Result<ProjectItem, CommandError> {
    restore_project_impl(&state, &id)
}

/// 彻底删除已归档项目：项目 + 全部记忆（含已归档）+ 候选 + 索引一并物理删除。
#[tauri::command]
pub async fn delete_project_permanent(
    state: State<'_, AppState>,
    id: String,
) -> Result<DeletedProjectStats, CommandError> {
    delete_project_permanent_impl(&state, &id)
}

/// 绑定项目工作空间标识。
#[tauri::command]
pub async fn bind_project_workspace(
    state: State<'_, AppState>,
    project_id: String,
    workspace_identifier: String,
) -> Result<ProjectItem, CommandError> {
    bind_project_workspace_impl(&state, &project_id, &workspace_identifier)
}

/// 解绑项目工作空间标识。
#[tauri::command]
pub async fn unbind_project_workspace(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectItem, CommandError> {
    unbind_project_workspace_impl(&state, &project_id)
}

pub fn list_projects_impl(state: &AppState, include_archived: bool) -> Result<Vec<ProjectItem>, CommandError> {
    Ok(state.projects.list(include_archived)?)
}

pub fn create_project_impl(state: &AppState, request: SaveProjectRequest) -> Result<ProjectItem, CommandError> {
    Ok(state.projects.create(&request)?)
}

pub fn update_project_impl(
    state: &AppState,
    id: &str,
    request: SaveProjectRequest,
) -> Result<ProjectItem, CommandError> {
    Ok(state.projects.update(id, &request)?)
}

pub fn archive_project_impl(state: &AppState, id: &str) -> Result<ProjectItem, CommandError> {
    Ok(state.projects.archive(id)?)
}

pub fn restore_project_impl(state: &AppState, id: &str) -> Result<ProjectItem, CommandError> {
    Ok(state.projects.restore(id)?)
}

pub fn delete_project_permanent_impl(state: &AppState, id: &str) -> Result<DeletedProjectStats, CommandError> {
    Ok(state.projects.delete_permanent(id)?)
}

pub fn bind_project_workspace_impl(
    state: &AppState,
    project_id: &str,
    workspace_identifier: &str,
) -> Result<ProjectItem, CommandError> {
    Ok(state.workspaces.bind_workspace(project_id, workspace_identifier)?)
}

pub fn unbind_project_workspace_impl(state: &AppState, project_id: &str) -> Result<ProjectItem, CommandError> {
    Ok(state.workspaces.unbind(project_id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::ErrorCode;

    fn state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        (AppState::build(database_path).unwrap(), temp)
    }

    fn request(name: &str) -> SaveProjectRequest {
        SaveProjectRequest {
            name: name.to_string(),
            description: "描述".to_string(),
            color: "#4f8cff".to_string(),
        }
    }

    #[test]
    fn project_lifecycle_covers_all_commands() {
        let (state, _temp) = state();
        assert!(list_projects_impl(&state, true).unwrap().is_empty());

        let created = create_project_impl(&state, request("忆栈")).unwrap();
        assert_eq!(created.name, "忆栈");
        assert_eq!(list_projects_impl(&state, false).unwrap().len(), 1);

        let updated = update_project_impl(&state, &created.id, request("忆栈2")).unwrap();
        assert_eq!(updated.name, "忆栈2");

        // 绑定 → 解绑工作空间（标识归一化取末段，与 C# WorkspaceIdentity 一致）。
        let bound = bind_project_workspace_impl(&state, &created.id, "C:\\demo\\workspace").unwrap();
        assert_eq!(bound.workspace_identifier.as_deref(), Some("workspace"));
        let unbound = unbind_project_workspace_impl(&state, &created.id).unwrap();
        assert!(unbound.workspace_identifier.is_none());

        let archived = archive_project_impl(&state, &created.id).unwrap();
        assert!(archived.is_archived);
        assert!(list_projects_impl(&state, false).unwrap().is_empty());
        let restored = restore_project_impl(&state, &created.id).unwrap();
        assert!(!restored.is_archived);
    }

    #[test]
    fn update_missing_project_returns_not_found() {
        let (state, _temp) = state();
        let error = update_project_impl(&state, "00000000-0000-0000-0000-000000000000", request("x")).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::ProjectNotFound.as_str());
    }
}
