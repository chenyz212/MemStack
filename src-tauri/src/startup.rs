//! 启动失败日志：对齐 C# `WriteStartupFailure` / `ClearStartupFailure`。
//!
//! 路径：`%LOCALAPPDATA%\MemStack\logs\startup-error.log`。
//! 格式：`{UTC ISO 8601}\n{失败摘要}`，覆写；启动成功后删除旧残留。

use std::io;
use std::path::{Path, PathBuf};

/// 返回 startup-error.log 路径（不创建目录）。
pub fn startup_error_log_path() -> io::Result<PathBuf> {
    Ok(memory_platform::logs_dir()?.join("startup-error.log"))
}

/// 写入启动失败摘要（静默容错：日志失败不阻断主流程）。
pub fn write_startup_failure(summary: &str) {
    let Ok(path) = startup_error_log_path() else {
        return;
    };
    write_startup_failure_to(&path, summary);
}

/// 启动成功后清理上次失败残留日志（静默容错）。
pub fn clear_startup_failure() {
    let Ok(path) = startup_error_log_path() else {
        return;
    };
    clear_startup_failure_at(&path);
}

fn write_startup_failure_to(path: &Path, summary: &str) {
    let Some(directory) = path.parent() else { return };
    let _ = std::fs::create_dir_all(directory);
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let _ = std::fs::write(path, format!("{timestamp}\n{summary}\n"));
}

fn clear_startup_failure_at(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_failure_roundtrip_writes_then_clears() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("logs").join("startup-error.log");
        // 预置旧残留：写入应覆写。
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "旧内容").unwrap();

        write_startup_failure_to(&path, "数据库打开失败");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("数据库打开失败"));
        assert!(!content.contains("旧内容"), "覆写而非追加");
        assert!(content.starts_with("20"), "首行应为 UTC 时间戳");

        clear_startup_failure_at(&path);
        assert!(!path.exists(), "成功启动后清理残留");
        // 幂等：再次清理不报错。
        clear_startup_failure_at(&path);
    }
}
