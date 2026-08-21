//! 候选记忆 Commands（§5.4，4 个）。
//!
//! 桌面固定 caller：`McpCallerContext(Guid.Empty, "桌面客户端", ReadWrite, null)`
//! （对齐 C# `ApiEndpoints.MapCandidateEndpoints`），不引入桌面端鉴权。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{MemoryCandidateItem, MemoryItem, SaveMemoryCandidateRequest};

/// 列出全部待确认候选（桌面 caller 无项目绑定 → 不过滤）。
#[tauri::command]
pub async fn list_memory_candidates(state: State<'_, AppState>) -> Result<Vec<MemoryCandidateItem>, CommandError> {
    list_memory_candidates_impl(&state)
}

/// 更新候选内容（`expectedVersion` 乐观锁在 `request` 内）。
#[tauri::command]
pub async fn update_memory_candidate(
    state: State<'_, AppState>,
    id: String,
    request: SaveMemoryCandidateRequest,
) -> Result<MemoryCandidateItem, CommandError> {
    update_memory_candidate_impl(&state, &id, &request)
}

/// 确认候选 → 转正为记忆。
#[tauri::command]
pub async fn confirm_memory_candidate(
    state: State<'_, AppState>,
    id: String,
    expected_version: i64,
) -> Result<MemoryItem, CommandError> {
    confirm_memory_candidate_impl(&state, &id, expected_version)
}

/// 拒绝候选。
#[tauri::command]
pub async fn reject_memory_candidate(
    state: State<'_, AppState>,
    id: String,
    expected_version: i64,
) -> Result<(), CommandError> {
    reject_memory_candidate_impl(&state, &id, expected_version)
}

pub fn list_memory_candidates_impl(state: &AppState) -> Result<Vec<MemoryCandidateItem>, CommandError> {
    Ok(state.candidates.list(&AppState::desktop_caller())?)
}

pub fn update_memory_candidate_impl(
    state: &AppState,
    id: &str,
    request: &SaveMemoryCandidateRequest,
) -> Result<MemoryCandidateItem, CommandError> {
    Ok(state.candidates.update(id, request)?)
}

pub fn confirm_memory_candidate_impl(
    state: &AppState,
    id: &str,
    expected_version: i64,
) -> Result<MemoryItem, CommandError> {
    Ok(state
        .candidates
        .confirm(id, expected_version, &AppState::desktop_caller())?)
}

pub fn reject_memory_candidate_impl(state: &AppState, id: &str, expected_version: i64) -> Result<(), CommandError> {
    Ok(state
        .candidates
        .reject(id, expected_version, &AppState::desktop_caller())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use memory_domain::{ErrorCode, MemoryScope};

    fn state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        (AppState::build(database_path).unwrap(), temp)
    }

    fn candidate_request(title: &str) -> SaveMemoryCandidateRequest {
        SaveMemoryCandidateRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title: title.to_string(),
            summary: String::new(),
            content: format!("候选内容-{title}"),
            memory_type: "NOTE".to_string(),
            keywords: vec![],
            tags: vec![],
            importance: 3,
            cloud_processing_allowed: false,
            expected_version: None,
        }
    }

    #[test]
    fn candidate_lifecycle_update_confirm_reject() {
        let (state, _temp) = state();
        // 经 MCP 提交通道生成候选（桌面进程没有提交入口，直接用服务）。
        let submitted = state.candidates.submit(&candidate_request("候选A"), "Codex").unwrap();
        assert_eq!(list_memory_candidates_impl(&state).unwrap().len(), 1);

        // 更新候选。
        let mut update = candidate_request("候选A-改");
        update.expected_version = Some(submitted.version);
        let updated = update_memory_candidate_impl(&state, &submitted.id, &update).unwrap();
        assert_eq!(updated.title, "候选A-改");

        // 确认 → 转正。
        let memory = confirm_memory_candidate_impl(&state, &submitted.id, updated.version).unwrap();
        assert_eq!(memory.title, "候选A-改");
        assert!(list_memory_candidates_impl(&state).unwrap().is_empty());

        // 拒绝路径。
        let second = state.candidates.submit(&candidate_request("候选B"), "Codex").unwrap();
        reject_memory_candidate_impl(&state, &second.id, second.version).unwrap();
        assert!(list_memory_candidates_impl(&state).unwrap().is_empty());
        let error = reject_memory_candidate_impl(&state, &second.id, second.version).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::MemoryCandidateNotFound.as_str());
    }

    #[test]
    fn confirm_with_stale_version_returns_conflict() {
        let (state, _temp) = state();
        let submitted = state.candidates.submit(&candidate_request("候选C"), "Codex").unwrap();
        let error = confirm_memory_candidate_impl(&state, &submitted.id, submitted.version + 1).unwrap_err();
        assert_eq!(error.code.as_str(), ErrorCode::MemoryCandidateVersionConflict.as_str());
        // 确认失败后候选仍在。
        assert_eq!(list_memory_candidates_impl(&state).unwrap().len(), 1);
    }

    /// 候选转正语义回归：created_source 记录候选来源（与 C# 及领域测试一致），
    /// 确认者身份不覆盖来源。
    #[test]
    fn confirmed_candidate_keeps_original_source() {
        let (state, _temp) = state();
        let submitted = state.candidates.submit(&candidate_request("候选D"), "Codex").unwrap();
        let memory = confirm_memory_candidate_impl(&state, &submitted.id, submitted.version).unwrap();
        assert_eq!(memory.created_source, "Codex");
    }
}
