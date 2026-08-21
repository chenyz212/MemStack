//! 数据访问层：rusqlite 连接、PRAGMA 配置、schema 识别、v1→v7 迁移链与跨进程迁移锁。

pub mod connection;
pub mod first_run_backup;
pub mod migrate;
pub mod migration_lock;
pub mod schema;

pub use connection::{init_pragmas, open_connection, open_read_only_connection, verify_fts5};
pub use first_run_backup::ensure_first_run_backup;
pub use migrate::{MigrationReport, run_migrations};
pub use migration_lock::{MIGRATION_LOCK_TIMEOUT, MIGRATION_MUTEX_NAME, open_initialized};
pub use schema::{detect_schema_version, is_desktop_schema, resolve_desktop_database_path};
