//! 日志轮换与总占用限制（§16）。
//!
//! 启动时检查托管日志：单文件超过 5MB 轮换为 `.1`（仅保留一代）；`logs` 目录
//! 总占用超过 50MB 时按最旧修改时间删除非当前文件。诊断辅助逻辑，任何失败
//! 静默忽略，不得阻断启动。

use std::path::Path;

/// 启动时纳入轮换检查的日志文件（当前活跃文件，裁剪时受保护）。
pub const MANAGED_LOG_FILES: &[&str] = &[
    "desktop.log",
    "mcp-stdio.log",
    "embedding-worker.log",
    "database-migration.log",
];

/// 当前活跃但由各自模块管理生命周期、不参与轮换的文件（裁剪时同样受保护）。
pub const PROTECTED_LOG_FILES: &[&str] = &["startup-error.log"];

/// 单文件轮换阈值（5MB）。
pub const ROTATE_THRESHOLD: u64 = 5 * 1024 * 1024;
/// 目录总占用上限（50MB）。
pub const DIRECTORY_BUDGET: u64 = 50 * 1024 * 1024;

/// 轮换策略（测试可注入小值）。
#[derive(Debug, Clone, Copy)]
pub struct RotationPolicy {
    /// 单文件超过该字节数时轮换为 `.1`。
    pub max_file_size: u64,
    /// 目录总占用超过该字节数时按最旧修改时间删除非当前文件。
    pub directory_budget: u64,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_file_size: ROTATE_THRESHOLD,
            directory_budget: DIRECTORY_BUDGET,
        }
    }
}

/// 生产入口：对 `%LOCALAPPDATA%\MemStack\logs` 执行轮换；失败静默。
pub fn rotate_logs() {
    let Ok(dir) = crate::logs_dir() else {
        return;
    };
    let _ = rotate_logs_in(&dir, &RotationPolicy::default());
}

/// 可测试核心：在指定目录执行轮换与裁剪。
pub fn rotate_logs_in(dir: &Path, policy: &RotationPolicy) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    // 1) 超大文件轮换为 `.1`（仅保留一代，旧 `.1` 被覆盖）。
    for name in MANAGED_LOG_FILES {
        let path = dir.join(name);
        if let Ok(metadata) = std::fs::metadata(&path)
            && metadata.len() > policy.max_file_size
        {
            let rotated = dir.join(format!("{name}.1"));
            let _ = std::fs::remove_file(&rotated);
            std::fs::rename(&path, &rotated)?;
        }
    }

    // 2) 目录总占用超限时按最旧修改时间删除非当前文件。
    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        entries.push((entry.path(), modified, metadata.len()));
    }
    let total: u64 = entries.iter().map(|(_, _, size)| *size).sum();
    if total <= policy.directory_budget {
        return Ok(());
    }

    let protected: Vec<std::path::PathBuf> = MANAGED_LOG_FILES
        .iter()
        .chain(PROTECTED_LOG_FILES)
        .map(|name| dir.join(name))
        .collect();
    // 最旧优先删除，直到回到预算内；当前活跃文件不动。
    entries.sort_by_key(|(_, modified, _)| *modified);
    let mut remaining = total;
    for (path, _, size) in entries {
        if remaining <= policy.directory_budget {
            break;
        }
        if protected.contains(&path) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            remaining = remaining.saturating_sub(size);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(max_file_size: u64, directory_budget: u64) -> RotationPolicy {
        RotationPolicy {
            max_file_size,
            directory_budget,
        }
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) {
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    /// 设置文件修改时间（std 稳定 API，免引入 filetime 依赖）。
    fn set_modified(dir: &Path, name: &str, at: std::time::SystemTime) {
        let file = std::fs::File::options().write(true).open(dir.join(name)).unwrap();
        file.set_modified(at).unwrap();
    }

    #[test]
    fn oversized_log_rotates_to_single_generation() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "mcp-stdio.log", "x".repeat(64).as_bytes());
        write(dir.path(), "mcp-stdio.log.1", b"old-generation");
        rotate_logs_in(dir.path(), &policy(10, u64::MAX)).unwrap();
        assert!(!dir.path().join("mcp-stdio.log").exists(), "超大文件应被轮换走");
        assert_eq!(
            std::fs::read(dir.path().join("mcp-stdio.log.1")).unwrap(),
            b"x".repeat(64),
            "旧一代 `.1` 被覆盖"
        );
    }

    #[test]
    fn under_threshold_files_untouched() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "desktop.log", b"small");
        rotate_logs_in(dir.path(), &policy(10, u64::MAX)).unwrap();
        assert_eq!(std::fs::read(dir.path().join("desktop.log")).unwrap(), b"small");
        assert!(!dir.path().join("desktop.log.1").exists());
    }

    #[test]
    fn budget_prune_deletes_oldest_non_current_first() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "mcp-stdio.log", b"current"); // 受保护
        write(dir.path(), "startup-error.log", b"keep"); // 受保护
        write(dir.path(), "old-a.log", b"aaaa"); // 最旧
        write(dir.path(), "old-b.log", b"bb");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        set_modified(dir.path(), "old-a.log", old);
        set_modified(dir.path(), "old-b.log", old + std::time::Duration::from_secs(60));

        // 总占用 7+4+4+2=17B；预算 13B → 删最旧的 old-a（4B）即达标。
        rotate_logs_in(dir.path(), &policy(u64::MAX, 13)).unwrap();
        assert!(!dir.path().join("old-a.log").exists(), "最旧文件应被删除");
        assert!(dir.path().join("old-b.log").exists());
        assert!(dir.path().join("mcp-stdio.log").exists(), "当前活跃文件受保护");
        assert!(dir.path().join("startup-error.log").exists(), "启动错误日志受保护");
    }

    #[test]
    fn missing_directory_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        assert!(rotate_logs_in(&dir.path().join("absent"), &policy(10, 10)).is_ok());
    }
}
