//! 记忆 Commands（§5.3，11 个）：列表 / 详情 / 创建 / 快速记录 / 更新 / 归档 /
//! 恢复 / 永久删除 / 历史版本列表 / 历史版本恢复 / 分类计数。
//!
//! 原 REST URL query 参数（分页 cursor / 筛选）改为结构化 `MemoryListQuery` 对象参数。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{
    CursorPage, MemoryFacets, MemoryItem, MemoryListQuery, MemoryRevisionItem, QuickCaptureRequest, SaveMemoryRequest,
};

/// 分页查询记忆列表（筛选与游标均在 `query` 内）。
#[tauri::command]
pub async fn list_memories(
    state: State<'_, AppState>,
    query: MemoryListQuery,
) -> Result<CursorPage<MemoryItem>, CommandError> {
    list_memories_impl(&state, &query)
}

/// 读取单条记忆。
#[tauri::command]
pub async fn get_memory(state: State<'_, AppState>, id: String) -> Result<MemoryItem, CommandError> {
    get_memory_impl(&state, &id)
}

/// 创建记忆（来源固定「桌面客户端」）。
#[tauri::command]
pub async fn create_memory(state: State<'_, AppState>, request: SaveMemoryRequest) -> Result<MemoryItem, CommandError> {
    create_memory_impl(&state, &request)
}

/// 快速记录（自动生成标题）。
#[tauri::command]
pub async fn quick_capture_memory(
    state: State<'_, AppState>,
    request: QuickCaptureRequest,
) -> Result<MemoryItem, CommandError> {
    quick_capture_memory_impl(&state, &request)
}

/// 更新记忆（`expectedVersion` 乐观锁在 `request` 内）。
#[tauri::command]
pub async fn update_memory(
    state: State<'_, AppState>,
    id: String,
    request: SaveMemoryRequest,
) -> Result<MemoryItem, CommandError> {
    update_memory_impl(&state, &id, &request)
}

/// 归档记忆。
#[tauri::command]
pub async fn archive_memory(state: State<'_, AppState>, id: String) -> Result<MemoryItem, CommandError> {
    archive_memory_impl(&state, &id)
}

/// 恢复记忆。
#[tauri::command]
pub async fn restore_memory(state: State<'_, AppState>, id: String) -> Result<MemoryItem, CommandError> {
    restore_memory_impl(&state, &id)
}

/// 永久删除记忆（不可恢复）。
#[tauri::command]
pub async fn delete_memory_permanently(state: State<'_, AppState>, id: String) -> Result<(), CommandError> {
    delete_memory_permanently_impl(&state, &id)
}

/// 历史版本列表（仅保留上一版快照）。
#[tauri::command]
pub async fn list_memory_revisions(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<MemoryRevisionItem>, CommandError> {
    list_memory_revisions_impl(&state, &id)
}

/// 恢复到指定历史版本。
#[tauri::command]
pub async fn restore_memory_revision(
    state: State<'_, AppState>,
    id: String,
    version: i64,
) -> Result<MemoryItem, CommandError> {
    restore_memory_revision_impl(&state, &id, version)
}

/// 记忆分类与项目计数。
#[tauri::command]
pub async fn get_memory_facets(state: State<'_, AppState>) -> Result<MemoryFacets, CommandError> {
    get_memory_facets_impl(&state)
}

pub fn list_memories_impl(state: &AppState, query: &MemoryListQuery) -> Result<CursorPage<MemoryItem>, CommandError> {
    Ok(state.memories.list(query)?)
}

pub fn get_memory_impl(state: &AppState, id: &str) -> Result<MemoryItem, CommandError> {
    Ok(state.memories.get(id)?)
}

pub fn create_memory_impl(state: &AppState, request: &SaveMemoryRequest) -> Result<MemoryItem, CommandError> {
    Ok(state.memories.create(request)?)
}

pub fn quick_capture_memory_impl(state: &AppState, request: &QuickCaptureRequest) -> Result<MemoryItem, CommandError> {
    Ok(state.memories.quick_capture(request)?)
}

pub fn update_memory_impl(state: &AppState, id: &str, request: &SaveMemoryRequest) -> Result<MemoryItem, CommandError> {
    Ok(state.memories.update(id, request)?)
}

pub fn archive_memory_impl(state: &AppState, id: &str) -> Result<MemoryItem, CommandError> {
    Ok(state.memories.archive(id)?)
}

pub fn restore_memory_impl(state: &AppState, id: &str) -> Result<MemoryItem, CommandError> {
    Ok(state.memories.restore(id)?)
}

pub fn delete_memory_permanently_impl(state: &AppState, id: &str) -> Result<(), CommandError> {
    Ok(state.memories.delete_permanently(id)?)
}

pub fn list_memory_revisions_impl(state: &AppState, id: &str) -> Result<Vec<MemoryRevisionItem>, CommandError> {
    Ok(state.memories.list_revisions(id)?)
}

pub fn restore_memory_revision_impl(state: &AppState, id: &str, version: i64) -> Result<MemoryItem, CommandError> {
    Ok(state.memories.restore_revision(id, version)?)
}

pub fn get_memory_facets_impl(state: &AppState) -> Result<MemoryFacets, CommandError> {
    Ok(state.memories.get_facets()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_domain::{ErrorCode, MemoryScope};

    fn state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        (AppState::build(database_path).unwrap(), temp)
    }

    fn request(title: &str) -> SaveMemoryRequest {
        SaveMemoryRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title: title.to_string(),
            summary: String::new(),
            content: format!("内容-{title}"),
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
    fn memory_lifecycle_covers_all_commands() {
        let (state, _temp) = state();
        // 创建 → 读取 → 列表 → 更新 → 归档 → 恢复 → 永久删除。
        let created = create_memory_impl(&state, &request("标题A")).unwrap();
        assert_eq!(created.created_source, "桌面客户端");

        let loaded = get_memory_impl(&state, &created.id).unwrap();
        assert_eq!(loaded.title, "标题A");

        let mut update = request("标题B");
        update.expected_version = Some(created.version);
        let updated = update_memory_impl(&state, &created.id, &update).unwrap();
        assert_eq!(updated.title, "标题B");

        let page = list_memories_impl(
            &state,
            &MemoryListQuery {
                scope: None,
                project_id: None,
                status: None,
                is_favorite: None,
                is_pinned: None,
                memory_type: None,
                tag: None,
                importance_min: None,
                cursor: None,
                size: 30,
            },
        )
        .unwrap();
        assert_eq!(page.items.len(), 1);

        // 历史版本：更新前的内容成为上一版快照。
        let revisions = list_memory_revisions_impl(&state, &created.id).unwrap();
        assert_eq!(revisions.len(), 1);
        let restored = restore_memory_revision_impl(&state, &created.id, revisions[0].version).unwrap();
        assert_eq!(restored.title, "标题A");

        let archived = archive_memory_impl(&state, &created.id).unwrap();
        assert_eq!(archived.status, memory_domain::MemoryStatus::Archived);
        let restored_memory = restore_memory_impl(&state, &created.id).unwrap();
        assert_eq!(restored_memory.status, memory_domain::MemoryStatus::Active);

        let facets = get_memory_facets_impl(&state).unwrap();
        assert_eq!(facets.all_count, 1);

        // 永久删除仅允许作用于已归档记忆（对齐 C# 语义）。
        archive_memory_impl(&state, &created.id).unwrap();
        delete_memory_permanently_impl(&state, &created.id).unwrap();
        let error = get_memory_impl(&state, &created.id).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::MemoryNotFound.as_str());
    }

    #[test]
    fn quick_capture_generates_title_and_defaults() {
        let (state, _temp) = state();
        let memory = quick_capture_memory_impl(
            &state,
            &QuickCaptureRequest {
                content: "第一行内容\n第二行".to_string(),
            },
        )
        .unwrap();
        assert_eq!(memory.title, "第一行内容");
        assert_eq!(memory.memory_type, "NOTE");
        assert_eq!(memory.importance, 3);
    }

    #[test]
    fn stale_version_update_returns_conflict() {
        let (state, _temp) = state();
        let created = create_memory_impl(&state, &request("标题")).unwrap();
        let mut update = request("新标题");
        update.expected_version = Some(created.version + 100);
        let error = update_memory_impl(&state, &created.id, &update).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::MemoryVersionConflict.as_str());
        // CommandError JSON 形状与 C# 对齐（details 空对象）。
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["details"], serde_json::json!({}));
    }
}
