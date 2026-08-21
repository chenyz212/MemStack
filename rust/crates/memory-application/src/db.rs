//! 数据库入口：统一的路径持有与连接打开。
//!
//! 第二轮连接管理从简（决策 2）：每操作开新连接，WAL 与 busy_timeout
//! 已由 `memory_storage::open_connection` 就绪；连接池化（桌面 4/MCP 2）属阶段 6。

use std::path::PathBuf;

use memory_domain::BusinessError;
use rusqlite::Connection;

/// 数据库定位与连接工厂。
#[derive(Debug, Clone)]
pub struct Database {
    path: PathBuf,
}

impl Database {
    /// 以指定 SQLite 文件路径创建数据库入口。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 数据库文件绝对/相对路径。
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 打开读写连接（含 foreign_keys/busy_timeout PRAGMA）。
    pub fn open(&self) -> Result<Connection, BusinessError> {
        memory_storage::open_connection(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_opens_with_pragmas() {
        let directory = std::env::temp_dir().join(format!("memapp-db-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("app.db");
        let _ = std::fs::remove_file(&path);
        let database = Database::new(&path);
        let connection = database.open().unwrap();
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);
        drop(connection);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&directory);
    }
}
