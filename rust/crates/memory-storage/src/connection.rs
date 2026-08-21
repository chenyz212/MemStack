//! SQLite 连接管理：PRAGMA 配置与 FTS5 编译验证。
//!
//! 连接约定与 C# `MemoryDatabase` 完全对齐：
//! - 每次打开连接：`PRAGMA foreign_keys = ON;`、`PRAGMA busy_timeout = 5000;`
//! - 初始化（建库/升级）时：`PRAGMA journal_mode = WAL;`、`PRAGMA synchronous = NORMAL;`

use memory_domain::{BusinessError, ErrorCode};
use rusqlite::Connection;

fn busy_timeout_error(error: rusqlite::Error) -> BusinessError {
    if let rusqlite::Error::SqliteFailure(ffi, _) = &error
        && (ffi.code == rusqlite::ErrorCode::DatabaseBusy || ffi.code == rusqlite::ErrorCode::DatabaseLocked)
    {
        return BusinessError::new(ErrorCode::DatabaseBusy);
    }
    BusinessError::with_message(ErrorCode::InternalError, format!("数据库错误：{error}"))
}

/// 以读写方式打开数据库连接，并应用每次连接必需的 PRAGMA。
pub fn open_connection(path: &std::path::Path) -> Result<Connection, BusinessError> {
    let connection = Connection::open(path).map_err(busy_timeout_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(busy_timeout_error)?;
    connection
        .busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(busy_timeout_error)?;
    Ok(connection)
}

/// 以只读方式打开数据库连接，并应用每次连接必需的 PRAGMA。
pub fn open_read_only_connection(path: &std::path::Path) -> Result<Connection, BusinessError> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(busy_timeout_error)?;
    connection
        .busy_timeout(std::time::Duration::from_millis(5000))
        .map_err(busy_timeout_error)?;
    Ok(connection)
}

/// 初始化（建库或升级）场景下执行 WAL 与同步策略设置。
///
/// WAL 确保逻辑：journal_mode 切换（DELETE→WAL）要求独占访问，并发连接同时
/// 切换时 SQLite 立即返回 BUSY（不经过 busy handler），故这里幂等探测 +
/// 短重试——先由任一进程完成转换，其余进程探测到 WAL 后直接通过。
pub fn init_pragmas(connection: &Connection) -> Result<(), BusinessError> {
    ensure_wal(connection)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(busy_timeout_error)?;
    Ok(())
}

fn ensure_wal(connection: &Connection) -> Result<(), BusinessError> {
    for _ in 0..50 {
        let current: String = connection
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .map_err(busy_timeout_error)?;
        if current.eq_ignore_ascii_case("wal") {
            return Ok(());
        }
        let attempted: Result<String, rusqlite::Error> =
            connection.query_row("PRAGMA journal_mode = WAL;", [], |row| row.get(0));
        match attempted {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(_) => continue,
            Err(error) => {
                let busy = matches!(
                    &error,
                    rusqlite::Error::SqliteFailure(ffi, _)
                        if ffi.code == rusqlite::ErrorCode::DatabaseBusy
                            || ffi.code == rusqlite::ErrorCode::DatabaseLocked
                );
                if busy {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
                return Err(busy_timeout_error(error));
            }
        }
    }
    Err(BusinessError::with_message(
        ErrorCode::DatabaseBusy,
        "等待数据库启用 WAL 超时",
    ))
}

/// 验证当前 rusqlite 静态链接的 SQLite 编译启用了 FTS5。
///
/// 这是第一周高风险验证点：失败必须当场解决（启用捆绑源码的 FTS5 开关），
/// 不允许静默跳过。
pub fn verify_fts5(connection: &Connection) -> Result<(), BusinessError> {
    let options: Vec<String> = connection
        .prepare("PRAGMA compile_options;")
        .and_then(|mut statement| statement.query_map([], |row| row.get::<_, String>(0))?.collect())
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("读取编译选项失败：{error}")))?;
    let enabled = options.iter().any(|option| option.contains("ENABLE_FTS5"));
    if enabled {
        Ok(())
    } else {
        Err(BusinessError::with_message(
            ErrorCode::DatabaseIncompatible,
            "当前 SQLite 编译未启用 FTS5，无法支持中文全文检索",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_sqlite_enables_fts5() {
        let connection = Connection::open_in_memory().unwrap();
        verify_fts5(&connection).expect("bundled SQLite 必须启用 FTS5");
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE VIRTUAL TABLE t USING fts5(content);", [])
            .expect("FTS5 虚拟表必须可创建");
    }

    #[test]
    fn open_connection_sets_required_pragmas() {
        let directory = std::env::temp_dir().join(format!("memstorage-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("pragma.db");
        let _ = std::fs::remove_file(&path);
        let connection = open_connection(&path).unwrap();
        init_pragmas(&connection).unwrap();
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys;", [], |row| row.get(0))
            .unwrap();
        let busy_timeout: i64 = connection
            .query_row("PRAGMA busy_timeout;", [], |row| row.get(0))
            .unwrap();
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5000);
        assert_eq!(journal_mode.to_lowercase(), "wal");
        drop(connection);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&directory);
    }
}
