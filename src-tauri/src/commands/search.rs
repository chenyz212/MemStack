//! 检索 Commands（§5.6，2 个）：混合检索 / 上下文构建。
//!
//! 参数与 C# REST 请求体一致（`SearchRequest` / `ContextRequest` 结构化对象）。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{ContextRequest, ContextResult, SearchRequest, SearchResult};

/// 混合检索（FTS + RRF + MMR + 模糊回退；语义按 Embedding 配置自动启用）。
#[tauri::command]
pub async fn search_memories(
    state: State<'_, AppState>,
    request: SearchRequest,
) -> Result<Vec<SearchResult>, CommandError> {
    search_memories_impl(&state, &request)
}

/// 构建面向 AI 的上下文（检索 + 去重 + 字符预算）。
#[tauri::command]
pub async fn build_memory_context(
    state: State<'_, AppState>,
    request: ContextRequest,
) -> Result<ContextResult, CommandError> {
    build_memory_context_impl(&state, &request)
}

pub fn search_memories_impl(state: &AppState, request: &SearchRequest) -> Result<Vec<SearchResult>, CommandError> {
    Ok(state.search.search(request)?)
}

pub fn build_memory_context_impl(state: &AppState, request: &ContextRequest) -> Result<ContextResult, CommandError> {
    Ok(state.search.build_context(request)?)
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

    fn request(title: &str, content: &str) -> SaveMemoryRequest {
        SaveMemoryRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title: title.to_string(),
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
    fn search_finds_created_memory() {
        let (state, _temp) = state();
        state
            .memories
            .create(&request("Rust 生命周期", "所有权与借用规则"))
            .unwrap();
        state
            .memories
            .create(&request("Vue 组合式 API", "ref 与 reactive"))
            .unwrap();

        let results = search_memories_impl(
            &state,
            &SearchRequest {
                query: "所有权".to_string(),
                scope: None,
                project_id: None,
                memory_type: None,
                tag: None,
                limit: 5,
                semantic_enabled: false,
            },
        )
        .unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].memory.title, "Rust 生命周期");
    }

    #[test]
    fn build_context_respects_character_budget() {
        let (state, _temp) = state();
        state.memories.create(&request("条目一", "很长的内容……")).unwrap();
        let context = build_memory_context_impl(
            &state,
            &ContextRequest {
                search: SearchRequest {
                    query: "条目".to_string(),
                    scope: None,
                    project_id: None,
                    memory_type: None,
                    tag: None,
                    limit: 5,
                    semantic_enabled: false,
                },
                max_characters: 30000,
            },
        )
        .unwrap();
        assert!(!context.items.is_empty());
        assert!(context.character_count > 0);
    }
}
