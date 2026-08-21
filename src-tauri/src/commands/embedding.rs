//! Embedding Commands（§5.7，5 个）：配置读取 / 连接测试 / 保存 / 重建 / 状态。
//!
//! Worker 接线见 `state.rs::spawn_worker`（桌面进程唯一消费者，§10.1）：
//! Tauri setup 启动，退出时 `shutdown_worker` 停止；排队任务落库不丢。

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_domain::{
    EmbeddingSettingsView, EmbeddingStatus, EmbeddingTestResult, RebuildTicket, SaveEmbeddingSettingsRequest,
};

/// 读取 Embedding 配置（API Key 明文回显，界面端雾化展示）。
#[tauri::command]
pub async fn get_embedding_settings(state: State<'_, AppState>) -> Result<EmbeddingSettingsView, CommandError> {
    get_embedding_settings_impl(&state)
}

/// 测试 Embedding 配置连通性（不落盘）。
#[tauri::command]
pub async fn test_embedding_settings(
    state: State<'_, AppState>,
    request: SaveEmbeddingSettingsRequest,
) -> Result<EmbeddingTestResult, CommandError> {
    test_embedding_settings_impl(&state, &request)
}

/// 保存 Embedding 配置（内部先测试后保存）。
#[tauri::command]
pub async fn save_embedding_settings(
    state: State<'_, AppState>,
    request: SaveEmbeddingSettingsRequest,
) -> Result<EmbeddingSettingsView, CommandError> {
    save_embedding_settings_impl(&state, &request)
}

/// 排队全量重建向量任务。
#[tauri::command]
pub async fn rebuild_embeddings(state: State<'_, AppState>) -> Result<RebuildTicket, CommandError> {
    rebuild_embeddings_impl(&state)
}

/// 查询向量模式与待处理任务数。
#[tauri::command]
pub async fn get_embedding_status(state: State<'_, AppState>) -> Result<EmbeddingStatus, CommandError> {
    get_embedding_status_impl(&state)
}

pub fn get_embedding_settings_impl(state: &AppState) -> Result<EmbeddingSettingsView, CommandError> {
    Ok(state.embedding.get_settings()?)
}

pub fn test_embedding_settings_impl(
    state: &AppState,
    request: &SaveEmbeddingSettingsRequest,
) -> Result<EmbeddingTestResult, CommandError> {
    Ok(state.embedding.test(request)?)
}

pub fn save_embedding_settings_impl(
    state: &AppState,
    request: &SaveEmbeddingSettingsRequest,
) -> Result<EmbeddingSettingsView, CommandError> {
    Ok(state.embedding.save(request)?)
}

pub fn rebuild_embeddings_impl(state: &AppState) -> Result<RebuildTicket, CommandError> {
    Ok(state.embedding.queue_rebuild()?)
}

pub fn get_embedding_status_impl(state: &AppState) -> Result<EmbeddingStatus, CommandError> {
    Ok(state.embedding.get_status()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    fn state() -> (AppState, tempfile::TempDir) {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("test.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        (AppState::build(database_path).unwrap(), temp)
    }

    #[test]
    fn embedding_settings_roundtrip_with_plain_key() {
        let (state, _temp) = state();
        // 缺省：未配置。
        let settings = get_embedding_settings_impl(&state).unwrap();
        assert!(!settings.configured);
        assert_eq!(settings.api_key, "");

        // 保存（无效地址在 save 内部测试失败 → Err），改用直接写库验证掩码读取？
        // 不 —— save 先测试连通性，本测试无网络服务，期望失败并检查错误码透传。
        let request = SaveEmbeddingSettingsRequest {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "test-model".to_string(),
            api_key: "sk-test".to_string(),
            dimensions: 1024,
            enabled: true,
        };
        let result = save_embedding_settings_impl(&state, &request);
        // 本机 127.0.0.1:9 无服务：测试失败，保存被拒绝（EmbeddingTestFailed）。
        assert!(result.is_err());
        // 配置仍未落盘。
        let settings = get_embedding_settings_impl(&state).unwrap();
        assert!(!settings.configured);
    }

    #[test]
    fn embedding_status_reports_keyword_mode_initially() {
        let (state, _temp) = state();
        let status = get_embedding_status_impl(&state).unwrap();
        assert_eq!(status.mode, "KEYWORD");
        assert_eq!(status.pending_tasks, 0);
    }

    #[test]
    fn rebuild_queues_background_task() {
        let (state, _temp) = state();
        let ticket = rebuild_embeddings_impl(&state).unwrap();
        assert_eq!(ticket.status, "PENDING");
        let status = get_embedding_status_impl(&state).unwrap();
        assert_eq!(status.pending_tasks, 1);
    }
}
