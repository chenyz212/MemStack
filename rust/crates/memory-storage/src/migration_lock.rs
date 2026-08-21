//! 跨进程迁移保护：`Local\MemStack.Database.Migration` 命名互斥锁。
//!
//! 流程（迁移执行计划 §6.3）：打开连接并应用 PRAGMA → 快路径版本==7 直接返回；
//! 否则取命名互斥锁（等待上限 30 秒，超时返回 `DATABASE_BUSY`）→ 持锁后**重读**
//! `user_version`（另一进程可能已完成迁移）→ 仍低于 7 才执行迁移 → 释放锁。
//! 桌面进程与 MCP stdio 进程共用本入口，无特权进程。

use std::path::Path;
use std::time::Duration;

use memory_domain::{BusinessError, ErrorCode};
use memory_platform::{NamedMutex, WaitResult};
use rusqlite::Connection;

use crate::migrate::{MigrationReport, run_migrations};
use crate::schema::{SUPPORTED_SCHEMA_VERSION, detect_schema_version};

/// 迁移互斥锁名（`Local\` 前缀 = CurrentUser 会话作用域）。
pub const MIGRATION_MUTEX_NAME: &str = r"Local\MemStack.Database.Migration";

/// 迁移锁等待上限。
pub const MIGRATION_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// 打开数据库连接并确保结构升级到 [`SUPPORTED_SCHEMA_VERSION`]（多进程安全）。
pub fn open_initialized(path: &Path) -> Result<Connection, BusinessError> {
    let mut connection = crate::connection::open_connection(path)?;
    crate::connection::init_pragmas(&connection)?;
    if detect_schema_version(&connection)? == SUPPORTED_SCHEMA_VERSION {
        return Ok(connection);
    }

    let mutex = NamedMutex::create(MIGRATION_MUTEX_NAME)?;
    match mutex.wait(MIGRATION_LOCK_TIMEOUT.as_millis() as u32)? {
        WaitResult::Timeout => {
            return Err(BusinessError::with_message(
                ErrorCode::DatabaseBusy,
                "等待数据库结构升级锁超时，请稍后重试",
            ));
        }
        WaitResult::Acquired | WaitResult::Abandoned => {}
    }

    let result = (|| -> Result<Option<MigrationReport>, BusinessError> {
        // 持锁后重读版本：另一进程可能刚刚完成迁移。
        if detect_schema_version(&connection)? == SUPPORTED_SCHEMA_VERSION {
            return Ok(None);
        }
        let report = run_migrations(&mut connection)?;
        Ok(Some(report))
    })();

    let _ = mutex.release();
    match result? {
        None => {}
        Some(report) => append_migration_log(&report),
    }
    Ok(connection)
}

/// 追加迁移日志到 `%LOCALAPPDATA%\MemStack\logs\database-migration.log`；失败静默。
fn append_migration_log(report: &MigrationReport) {
    use std::io::Write as _;

    let Ok(logs_dir) = memory_platform::logs_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&logs_dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir.join("database-migration.log"))
    {
        let _ = writeln!(
            file,
            "[pid={}] schema {} -> {} steps={:?}",
            std::process::id(),
            report.from_version,
            report.to_version,
            report.steps
        );
    }
}
