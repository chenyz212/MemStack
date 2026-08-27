//! Windows 平台能力：LocalAppData 路径、DPAPI 凭据保护、单实例锁和日志目录。
//!
//! 第一轮实现路径解析与 DPAPI（T6）；单实例锁等桌面能力属阶段 7。

pub mod dpapi;
pub mod log_rotation;
pub mod named_mutex;
pub mod paths;
pub mod process_tree;

pub use dpapi::{DpapiError, protect, unprotect};
pub use named_mutex::{NamedMutex, WaitResult};
pub use paths::{
    backup_dir, data_dir, legacy_local_app_data_dir, local_app_data_dir, logs_dir, migrate_legacy_dir_if_needed,
};
