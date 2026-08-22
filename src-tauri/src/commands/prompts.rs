//! AI 全局提示词 Commands（执行计划 §21.5：查看 / 复制 / 差异 / 安装）。
//!
//! - 预览与安装使用同一生成结果（复制内容与安装内容一致）。
//! - 只有点击安装才写入；写入前备份、写入后验证、幂等可重装。
//! - 不支持自动安装的客户端仅返回可复制文本（can_install=false）。

use std::path::PathBuf;

use tauri::State;

use crate::error::CommandError;
use crate::state::AppState;
use memory_application::ai_prompt::{self, PromptReport};

/// 预览指定 AI 客户端的全局提示词（只读，不落盘）。
#[tauri::command]
pub async fn get_ai_prompt_preview(
    state: State<'_, AppState>,
    client_type: String,
) -> Result<PromptReport, CommandError> {
    let _ = &state;
    get_ai_prompt_preview_impl(&client_type)
}

/// 安装指定 AI 客户端的全局提示词（用户确认后调用；备份 → 写入 → 验证）。
#[tauri::command]
pub async fn install_ai_prompt(state: State<'_, AppState>, client_type: String) -> Result<PromptReport, CommandError> {
    let _ = &state;
    install_ai_prompt_impl(&client_type)
}

pub fn get_ai_prompt_preview_impl(client_type: &str) -> Result<PromptReport, CommandError> {
    let (home, appdata) = user_directories()?;
    Ok(ai_prompt::preview_prompt(client_type, &home, &appdata)?)
}

pub fn install_ai_prompt_impl(client_type: &str) -> Result<PromptReport, CommandError> {
    let (home, appdata) = user_directories()?;
    Ok(ai_prompt::install_prompt(client_type, &home, &appdata)?)
}

/// 读取安装所需用户目录（%USERPROFILE% 与 %APPDATA%）。
fn user_directories() -> Result<(PathBuf, PathBuf), CommandError> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).ok_or_else(|| {
        CommandError::with_message(memory_domain::ErrorCode::InternalError, "未定义 USERPROFILE 环境变量")
    })?;
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from).ok_or_else(|| {
        CommandError::with_message(memory_domain::ErrorCode::InternalError, "未定义 APPDATA 环境变量")
    })?;
    Ok((home, appdata))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 环境变量类测试的串行锁（避免并行 env 竞态）。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn preview_and_install_roundtrip() {
        let home = tempfile::tempdir().unwrap();
        let appdata = tempfile::tempdir().unwrap();
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY：测试串行持有 ENV_LOCK，无并发访问环境变量。
        unsafe {
            std::env::set_var("USERPROFILE", home.path());
            std::env::set_var("APPDATA", appdata.path());
        }

        let preview = get_ai_prompt_preview_impl("codex").unwrap();
        assert!(preview.can_install);
        assert!(!preview.installed);
        assert!(preview.prompt_text.contains("memstack"));
        // 提示词不含敏感信息。
        assert!(!preview.prompt_text.contains(home.path().to_str().unwrap()));

        let installed = install_ai_prompt_impl("codex").unwrap();
        assert!(installed.installed);
        assert!(installed.config_path.is_some());
        // 安装后预览报告已安装。
        let after = get_ai_prompt_preview_impl("codex").unwrap();
        assert!(after.installed);

        // SAFETY：同上。
        unsafe {
            std::env::remove_var("USERPROFILE");
            std::env::remove_var("APPDATA");
        }
    }
}
