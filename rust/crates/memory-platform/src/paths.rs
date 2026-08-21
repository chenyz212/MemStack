//! `%LOCALAPPDATA%\MemStack` 目录解析。
//!
//! 数据目录与日志目录沿用 C# 版布局，保证升级兼容：
//! `%LOCALAPPDATA%\MemStack\data` 与 `%LOCALAPPDATA%\MemStack\logs`。
//!
//! 品牌升级历史：0.3.x 之前目录名为 `UnifiedAiMemory`，0.4.0 起统一为 `MemStack`。
//! 首次启动时若新目录不存在但旧目录存在，会自动整体迁移（见 `migrate_legacy_dir_if_needed`）。

use std::path::PathBuf;

/// 当前品牌目录名（0.4.0 起统一为 MemStack）。
const APP_DATA_DIRECTORY: &str = "MemStack";

/// 旧版品牌目录名（0.3.x 及之前），用于自动迁移识别。
const LEGACY_APP_DATA_DIRECTORY: &str = "UnifiedAiMemory";

/// 返回 `%LOCALAPPDATA%\MemStack` 根目录（不创建）。
pub fn local_app_data_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other("未定义 LOCALAPPDATA 环境变量"))?;
    Ok(base.join(APP_DATA_DIRECTORY))
}

/// 返回旧版根目录 `%LOCALAPPDATA%\UnifiedAiMemory`（仅用于迁移识别，不创建）。
pub fn legacy_local_app_data_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other("未定义 LOCALAPPDATA 环境变量"))?;
    Ok(base.join(LEGACY_APP_DATA_DIRECTORY))
}

/// 返回数据目录 `%LOCALAPPDATA%\MemStack\data`（不创建）。
pub fn data_dir() -> std::io::Result<PathBuf> {
    Ok(local_app_data_dir()?.join("data"))
}

/// 返回日志目录 `%LOCALAPPDATA%\MemStack\logs`（不创建）。
pub fn logs_dir() -> std::io::Result<PathBuf> {
    Ok(local_app_data_dir()?.join("logs"))
}

/// 返回备份目录 `%LOCALAPPDATA%\MemStack\backup`（不创建）。
pub fn backup_dir() -> std::io::Result<PathBuf> {
    Ok(local_app_data_dir()?.join("backup"))
}

/// 品牌升级自动迁移：若新目录 `MemStack` 不存在但旧目录 `UnifiedAiMemory` 存在，
/// 整体重命名旧目录为新目录。新目录已存在或旧目录不存在时静默跳过。
///
/// 注意：此函数应在应用启动最早阶段（数据库打开之前）调用。若旧目录中有文件被
/// 其他进程占用（例如上一版本进程尚未退出），重命名会失败并返回错误，调用方应
/// 提示用户退出旧版本后重启。
pub fn migrate_legacy_dir_if_needed() -> std::io::Result<()> {
    let new_dir = local_app_data_dir()?;
    let legacy_dir = legacy_local_app_data_dir()?;
    if new_dir.exists() {
        return Ok(());
    }
    if !legacy_dir.exists() {
        return Ok(());
    }
    std::fs::rename(&legacy_dir, &new_dir).map_err(|error| {
        std::io::Error::other(format!(
            "品牌升级目录迁移失败：{} -> {}（{error}）",
            legacy_dir.display(),
            new_dir.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_data_paths_use_memstack_layout() {
        let base = local_app_data_dir().unwrap();
        assert!(base.ends_with(APP_DATA_DIRECTORY));
        assert_eq!(APP_DATA_DIRECTORY, "MemStack");
        assert!(data_dir().unwrap().ends_with("data"));
        assert!(logs_dir().unwrap().ends_with("logs"));
    }

    #[test]
    fn legacy_dir_points_to_unified_ai_memory() {
        let legacy = legacy_local_app_data_dir().unwrap();
        assert!(legacy.ends_with(LEGACY_APP_DATA_DIRECTORY));
        assert_eq!(LEGACY_APP_DATA_DIRECTORY, "UnifiedAiMemory");
    }
}
