//! 首次运行自动备份（§17.1 发布安全网）。
//!
//! Rust 版第一次写入生产数据库前，使用 SQLite Online Backup API 创建一致性
//! 备份（禁止在 WAL 未处理时直接复制 `.db` 文件）。全流程：
//!
//! ```text
//! marker 不存在 → Online Backup → 写 .sha256 校验和
//! → 打开备份执行 integrity_check → 复算校验和比对
//! → 裁剪旧备份（保留 2 份）→ 写 marker → 放行启动
//! ```
//!
//! 任一步失败返回错误并阻断启动（不写 marker，下次启动重试）。
//! `MEMSTACK_DB_PATH` 显式覆盖的测试/样本场景由调用方跳过本安全网。

use std::path::{Path, PathBuf};

use memory_domain::{BusinessError, ErrorCode};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// 首次运行标记文件名（位于备份目录）。
pub const MARKER_FILE: &str = "rust-first-run.marker";
/// 备份文件名前缀。
pub const BACKUP_PREFIX: &str = "pre-rust-";
/// 备份保留份数（更早的连同校验文件删除）。
pub const KEEP_BACKUPS: usize = 2;

/// 生产入口：备份目录取 `%LOCALAPPDATA%\MemStack\backup`。
pub fn ensure_first_run_backup(database_path: &Path) -> Result<Option<PathBuf>, BusinessError> {
    let backup_dir = memory_platform::backup_dir().map_err(|error| io_error("解析备份目录失败", error))?;
    ensure_first_run_backup_in(&backup_dir, database_path)
}

/// 可测试核心：在指定备份目录执行首启备份；返回创建的备份路径（跳过时 None）。
pub fn ensure_first_run_backup_in(backup_dir: &Path, database_path: &Path) -> Result<Option<PathBuf>, BusinessError> {
    let marker = backup_dir.join(MARKER_FILE);
    if marker.exists() {
        return Ok(None);
    }
    // 全新安装（无数据库文件）：无内容可备份，直接写 marker 完成"首次"判定。
    if !database_path.exists() {
        std::fs::create_dir_all(backup_dir).map_err(|error| io_error("创建备份目录失败", error))?;
        write_marker(&marker)?;
        return Ok(None);
    }
    std::fs::create_dir_all(backup_dir).map_err(|error| io_error("创建备份目录失败", error))?;

    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S%3f");
    let backup_path = backup_dir.join(format!("{BACKUP_PREFIX}{timestamp}.db"));
    run_online_backup(database_path, &backup_path)?;

    // 校验和文件与完整性检查全部通过才允许继续。
    let checksum = sha256_file(&backup_path)?;
    let checksum_path = checksum_sibling(&backup_path);
    std::fs::write(&checksum_path, format!("{checksum}\n")).map_err(|error| io_error("写入校验和失败", error))?;
    verify_backup(&backup_path, &checksum)?;

    prune_old_backups(backup_dir)?;
    write_marker(&marker)?;
    Ok(Some(backup_path))
}

/// SQLite Online Backup API：源库含 WAL 内容的一致性快照。
fn run_online_backup(source: &Path, destination: &Path) -> Result<(), BusinessError> {
    let source = crate::connection::open_connection(source)?;
    let mut target = Connection::open(destination).map_err(|error| sqlite_error("打开备份目标失败", error))?;
    let backup =
        rusqlite::backup::Backup::new(&source, &mut target).map_err(|error| sqlite_error("初始化备份失败", error))?;
    backup
        .run_to_completion(100, std::time::Duration::from_millis(50), None)
        .map_err(|error| sqlite_error("执行备份失败", error))?;
    Ok(())
}

/// 打开备份执行 `PRAGMA integrity_check`，并复算校验和比对（防写入损坏）。
fn verify_backup(backup_path: &Path, expected_checksum: &str) -> Result<(), BusinessError> {
    let connection = Connection::open(backup_path).map_err(|error| sqlite_error("打开备份校验失败", error))?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check;", [], |row| row.get(0))
        .map_err(|error| sqlite_error("备份完整性检查失败", error))?;
    if integrity != "ok" {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            format!("备份完整性检查未通过：{integrity}"),
        ));
    }
    drop(connection);
    let actual = sha256_file(backup_path)?;
    if !actual.eq_ignore_ascii_case(expected_checksum) {
        return Err(BusinessError::with_message(
            ErrorCode::InternalError,
            "备份校验和比对失败，备份文件可能损坏",
        ));
    }
    Ok(())
}

/// 计算文件 SHA-256（十六进制小写）。
fn sha256_file(path: &Path) -> Result<String, BusinessError> {
    let bytes = std::fs::read(path).map_err(|error| io_error("读取备份失败", error))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// 备份文件对应的 `.sha256` 路径（`pre-rust-x.db` → `pre-rust-x.db.sha256`）。
fn checksum_sibling(backup_path: &Path) -> PathBuf {
    let mut name = backup_path.file_name().unwrap_or_default().to_os_string();
    name.push(".sha256");
    backup_path.with_file_name(name)
}

/// 裁剪旧备份：按文件名（含时间戳，字典序即时间序）倒序保留 [`KEEP_BACKUPS`] 份。
fn prune_old_backups(backup_dir: &Path) -> Result<(), BusinessError> {
    let mut backups: Vec<PathBuf> = std::fs::read_dir(backup_dir)
        .map_err(|error| io_error("读取备份目录失败", error))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| extension == "db")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(BACKUP_PREFIX))
        })
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(KEEP_BACKUPS);
    for old in &backups[..excess] {
        let _ = std::fs::remove_file(old);
        let _ = std::fs::remove_file(checksum_sibling(old));
    }
    Ok(())
}

/// 写入首启 marker（内容为完成时间与源库路径，便于诊断）。
fn write_marker(marker: &Path) -> Result<(), BusinessError> {
    let content = format!(
        "completed={}\ndatabase={}\n",
        chrono::Utc::now().to_rfc3339(),
        std::env::args().next().unwrap_or_default()
    );
    std::fs::write(marker, content).map_err(|error| io_error("写入首启标记失败", error))
}

fn io_error(context: &str, error: std::io::Error) -> BusinessError {
    BusinessError::with_message(ErrorCode::InternalError, format!("{context}：{error}"))
}

fn sqlite_error(context: &str, error: rusqlite::Error) -> BusinessError {
    BusinessError::with_message(ErrorCode::InternalError, format!("{context}：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
        let data = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        (data, backups)
    }

    fn create_schema7_database(path: &Path) -> Connection {
        let connection = crate::open_initialized(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE demo (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO demo (value) VALUES ('忆栈'), ('MemStack');",
            )
            .unwrap();
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        connection
    }

    #[test]
    fn marker_present_skips_backup() {
        let (data, backups) = fixture();
        std::fs::create_dir_all(backups.path()).unwrap();
        std::fs::write(backups.path().join(MARKER_FILE), "done").unwrap();
        let database = data.path().join("memory.db");
        create_schema7_database(&database);
        let result = ensure_first_run_backup_in(backups.path(), &database).unwrap();
        assert!(result.is_none(), "marker 存在时应跳过");
        assert_eq!(std::fs::read_dir(backups.path()).unwrap().count(), 1);
    }

    #[test]
    fn missing_database_writes_marker_without_backup() {
        let (data, backups) = fixture();
        let database = data.path().join("memory.db");
        let result = ensure_first_run_backup_in(backups.path(), &database).unwrap();
        assert!(result.is_none(), "无数据库文件时不应产生备份");
        assert!(backups.path().join(MARKER_FILE).exists(), "应写 marker");
        assert!(
            !std::fs::read_dir(backups.path()).unwrap().any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "db"))
        );
    }

    #[test]
    fn first_run_creates_verified_backup_then_skips() {
        let (data, backups) = fixture();
        let database = data.path().join("memory.db");
        create_schema7_database(&database);

        let created = ensure_first_run_backup_in(backups.path(), &database)
            .unwrap()
            .expect("首次应创建备份");
        assert!(created.is_file(), "备份文件应存在");
        let checksum = checksum_sibling(&created);
        assert!(checksum.is_file(), "校验和文件应存在");
        assert!(backups.path().join(MARKER_FILE).exists(), "成功后应写 marker");

        // 备份可打开且数据一致。
        let backup = Connection::open(&created).unwrap();
        let count: i64 = backup
            .query_row("SELECT COUNT(*) FROM demo;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2, "备份数据应与源库一致");
        let integrity: String = backup
            .query_row("PRAGMA integrity_check;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");

        // 第二次调用跳过。
        let again = ensure_first_run_backup_in(backups.path(), &database).unwrap();
        assert!(again.is_none());
    }

    #[test]
    fn wal_active_database_backup_is_consistent() {
        let (data, backups) = fixture();
        let database = data.path().join("memory.db");
        // 保持源连接打开且 WAL 未 checkpoint：Online Backup 仍须得到一致快照。
        let live = create_schema7_database(&database);
        live.execute("INSERT INTO demo (value) VALUES ('wal-row');", [])
            .unwrap();

        let created = ensure_first_run_backup_in(backups.path(), &database)
            .unwrap()
            .expect("WAL 活跃时应能备份");
        let backup = Connection::open(&created).unwrap();
        let count: i64 = backup
            .query_row("SELECT COUNT(*) FROM demo;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 3, "WAL 中未 checkpoint 的写入也应进入备份");
    }

    #[test]
    fn prune_keeps_only_two_newest_backups() {
        let (data, backups) = fixture();
        let database = data.path().join("memory.db");
        create_schema7_database(&database);
        let dir = backups.path();
        // 预置 2 份更早的备份（含校验文件）。
        for name in ["pre-rust-20200101-000000000.db", "pre-rust-20200102-000000000.db"] {
            std::fs::write(dir.join(name), b"old").unwrap();
            std::fs::write(dir.join(format!("{name}.sha256")), b"old").unwrap();
        }

        ensure_first_run_backup_in(dir, &database).unwrap();
        let mut remaining: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(BACKUP_PREFIX) && name.ends_with(".db"))
            .collect();
        remaining.sort();
        assert_eq!(remaining.len(), KEEP_BACKUPS, "只保留 2 份备份");
        assert!(
            !remaining.iter().any(|name| name.contains("20200101")),
            "最旧的应被删除"
        );
        assert!(
            !dir.join("pre-rust-20200101-000000000.db.sha256").exists(),
            "校验文件一并删除"
        );
    }

    #[test]
    fn corrupt_source_blocks_startup_without_marker() {
        let (data, backups) = fixture();
        let database = data.path().join("memory.db");
        std::fs::write(&database, b"this is not a sqlite database").unwrap();

        let result = ensure_first_run_backup_in(backups.path(), &database);
        assert!(result.is_err(), "损坏源库必须返回错误");
        assert!(
            !backups.path().join(MARKER_FILE).exists(),
            "失败时不得写 marker（下次启动重试）"
        );
    }
}
