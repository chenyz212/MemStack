//! schema 识别与桌面数据库路径解析。
//!
//! 移植 C# `MemoryDatabase.ResolveDesktopDatabasePathAsync` / `IsDesktopSchemaAsync` 语义：
//! `memory.db` 优先；识别为旧 Java 结构时回退 `desktop-memory.db`；两者均不兼容时报
//! “桌面数据库结构不兼容”。

use std::path::{Path, PathBuf};

use memory_domain::{BusinessError, ErrorCode};
use rusqlite::Connection;

/// 主数据库文件名（与 C# `PrimaryFileName` 一致）。
pub const PRIMARY_FILE_NAME: &str = "memory.db";
/// 桌面回退数据库文件名（与 C# `DesktopFileName` 一致）。
pub const DESKTOP_FILE_NAME: &str = "desktop-memory.db";

/// 当前兼容的结构版本（v8：mcp_token 增加最近记忆活动字段）。
pub const SUPPORTED_SCHEMA_VERSION: i64 = 8;

fn incompatible() -> BusinessError {
    BusinessError::new(ErrorCode::DatabaseIncompatible)
}

/// 读取 `PRAGMA user_version`。
pub fn detect_schema_version(connection: &Connection) -> Result<i64, BusinessError> {
    connection
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("读取结构版本失败：{error}")))
}

/// 检查数据库是否为空库或桌面客户端结构（列检测规则与 C# 完全一致）。
pub fn is_desktop_schema(path: &Path) -> Result<bool, BusinessError> {
    let Ok(connection) = open_read_only(path) else {
        // 空文件或暂不可读时按 C# 行为交由上层决定；此处显式返回错误。
        return Err(incompatible());
    };
    let memory_columns = read_columns(&connection, "memory")?;
    if memory_columns.is_empty() {
        return Ok(true);
    }
    let project_columns = read_columns(&connection, "project")?;
    let required_memory_columns = [
        "id",
        "scope",
        "project_id",
        "title",
        "content",
        "is_favorite",
        "is_pinned",
        "cloud_processing_allowed",
        "version",
        "status",
        "content_checksum",
    ];
    let required_project_columns = ["id", "name", "color", "is_archived"];
    let memory_ok = required_memory_columns.iter().all(|column| {
        memory_columns
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(column))
    });
    let project_ok = required_project_columns.iter().all(|column| {
        project_columns
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(column))
    });
    Ok(memory_ok && project_ok)
}

fn open_read_only(path: &Path) -> Result<Connection, rusqlite::Error> {
    // 结构探测必须对 WAL 模式数据库（无 -shm 残留）也能打开：
    // 普通 READ_ONLY 打开 WAL 库时要求 -shm 存在，否则 SQLITE_CANTOPEN
    // （C# 现网 0.4.0 缺陷：旧 Java WAL 库探测失败导致客户端无法启动）。
    // immutable=1 跳过 wal/shm，只读快照语义，符合纯探测场景。
    let uri = format!(
        "file:{}?immutable=1",
        path.display().to_string().replace('?', "%3f").replace('#', "%23")
    );
    Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
}

fn read_columns(connection: &Connection, table: &str) -> Result<Vec<String>, BusinessError> {
    let sql = format!("PRAGMA table_info('{table}');");
    let Ok(mut statement) = connection.prepare(&sql) else {
        return Ok(Vec::new());
    };
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("读取表结构失败：{error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("读取表结构失败：{error}")))?;
    Ok(columns)
}

/// 解析数据目录中的桌面数据库路径，语义与 C# `ResolveDesktopDatabasePathAsync` 一致。
pub fn resolve_desktop_database_path(data_directory: &Path) -> Result<PathBuf, BusinessError> {
    std::fs::create_dir_all(data_directory)
        .map_err(|error| BusinessError::with_message(ErrorCode::InternalError, format!("创建数据目录失败：{error}")))?;
    let primary_path = data_directory.join(PRIMARY_FILE_NAME);
    let primary_usable = !primary_path.exists() || is_desktop_schema(&primary_path)?;
    if primary_usable {
        return Ok(primary_path);
    }
    let desktop_path = data_directory.join(DESKTOP_FILE_NAME);
    let desktop_usable = !desktop_path.exists() || is_desktop_schema(&desktop_path)?;
    if desktop_usable {
        return Ok(desktop_path);
    }
    Err(incompatible())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "memstack-schema-probe-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    /// 复现 C# 现网缺陷场景：WAL 模式库关闭后 wal/shm 不存在，只读结构探测必须仍可打开。
    #[test]
    fn schema_probe_opens_wal_database_without_shm_files() {
        let directory = temp_directory("wal-desktop");
        let path = directory.join("memory.db");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA journal_mode = WAL;
                     CREATE TABLE project (id TEXT PRIMARY KEY, name TEXT, color TEXT, is_archived INTEGER);
                     CREATE TABLE memory (id TEXT PRIMARY KEY, scope TEXT, project_id TEXT, title TEXT,
                         content TEXT, is_favorite INTEGER, is_pinned INTEGER,
                         cloud_processing_allowed INTEGER, version INTEGER, status TEXT,
                         content_checksum TEXT);",
                )
                .unwrap();
        }
        let _ = std::fs::remove_file(directory.join("memory.db-wal"));
        let _ = std::fs::remove_file(directory.join("memory.db-shm"));
        assert!(is_desktop_schema(&path).unwrap(), "无 shm 的 WAL 库探测必须成功");
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// 同场景下的旧 Java 结构：必须返回 false 触发回退，而不是打开错误。
    #[test]
    fn schema_probe_rejects_legacy_wal_database_without_shm_files() {
        let directory = temp_directory("wal-legacy");
        let path = directory.join("memory.db");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "PRAGMA journal_mode = WAL;
                     CREATE TABLE project (id TEXT PRIMARY KEY, name TEXT);
                     CREATE TABLE memory (id TEXT PRIMARY KEY, title TEXT, content TEXT);",
                )
                .unwrap();
        }
        let _ = std::fs::remove_file(directory.join("memory.db-wal"));
        let _ = std::fs::remove_file(directory.join("memory.db-shm"));
        assert!(!is_desktop_schema(&path).unwrap(), "旧结构必须返回 false 走回退");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
