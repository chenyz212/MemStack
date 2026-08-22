//! MemStack 领域契约：稳定实体、错误模型和序列化规则。
//!
//! 该模块禁止依赖 Tauri、MCP SDK、SQLite、Windows API 和 HTTP 客户端。

pub mod checksum;
pub mod cursor;
pub mod error;
pub mod models;
pub mod project_document;

pub use checksum::content_checksum;
pub use cursor::{MemoryCursor, decode_cursor, encode_cursor};
pub use error::{BusinessError, ErrorCode};
pub use models::*;
pub use project_document::*;
