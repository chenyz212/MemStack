//! SQLite 错误到业务错误的统一映射。
//!
//! 唯一约束冲突（SQLITE_CONSTRAINT，主错误码 19）在不同服务中映射为
//! 不同的业务码（如 PROJECT_NAME_EXISTS / MEMORY_DUPLICATE），由调用点
//! 通过 `unique_constraint_error` 提供目标错误；其余错误统一收敛为
//! DATABASE_BUSY（锁）或 INTERNAL_ERROR。

use memory_domain::{BusinessError, ErrorCode};

/// 将 rusqlite 错误映射为业务错误（不含唯一约束场景，见 `unique_constraint_error`）。
pub fn map_sqlite_error(error: rusqlite::Error) -> BusinessError {
    if let rusqlite::Error::SqliteFailure(ffi, _) = &error
        && (ffi.code == rusqlite::ErrorCode::DatabaseBusy || ffi.code == rusqlite::ErrorCode::DatabaseLocked)
    {
        return BusinessError::new(ErrorCode::DatabaseBusy);
    }
    BusinessError::with_message(ErrorCode::InternalError, format!("数据库错误：{error}"))
}

/// 若错误为唯一约束冲突（主错误码 19），返回调用点指定的业务错误；
/// 否则返回 `None`（调用点继续走 `map_sqlite_error`）。
///
/// C# 侧对应 `catch (SqliteException exception) when exception.SqliteErrorCode == 19`。
pub fn unique_constraint_error(error: &rusqlite::Error) -> Option<BusinessError> {
    if let rusqlite::Error::SqliteFailure(ffi, _) = error
        && ffi.code == rusqlite::ErrorCode::ConstraintViolation
    {
        return Some(BusinessError::new(ErrorCode::Conflict));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造带指定主错误码的 SQLite 失败（主码：5=BUSY，19=CONSTRAINT）。
    fn sqlite_failure(primary_code: i32) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(primary_code), Some("test".to_string()))
    }

    #[test]
    fn constraint_violation_is_detected() {
        let error = sqlite_failure(19);
        assert!(unique_constraint_error(&error).is_some());
        let mapped = unique_constraint_error(&error).unwrap();
        assert_eq!(mapped.code, ErrorCode::Conflict);
    }

    #[test]
    fn busy_error_maps_to_database_busy() {
        let error = sqlite_failure(5);
        assert_eq!(map_sqlite_error(error).code, ErrorCode::DatabaseBusy);
    }

    #[test]
    fn other_errors_map_to_internal() {
        let error = rusqlite::Error::QueryReturnedNoRows;
        assert_eq!(map_sqlite_error(error).code, ErrorCode::InternalError);
    }
}
