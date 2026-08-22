//! 业务用例层：MemoryService、ProjectService、SearchService 等。
//!
//! Tauri Command 与 MCP 工具只能调用该层，不得直接拼接 SQL。
//! 基础设施（db/clock/ids/tokenizer/sqlite_errors）供各服务共享。

pub mod ai_prompt;
pub mod candidate_service;
pub mod client_registration;
pub mod clock;
pub mod conclusion_card_service;
pub mod db;
pub mod embedding_service;
pub mod embedding_worker;
pub mod graph_service;
pub mod ids;
pub mod mcp_access;
pub mod mcp_connection_test;
pub mod memory_service;
pub mod overview_service;
pub mod project_document_fs;
pub mod project_document_service;
pub mod project_document_watcher;
pub mod project_service;
pub mod search_service;
pub mod sqlite_errors;
pub mod tokenizer;
pub mod workspace_identity;
pub mod workspace_service;

pub use clock::{Clock, FixedClock, SystemClock, format_storage_time};
pub use db::Database;
pub use ids::{FixedIdGenerator, GuidGenerator, IdGenerator};
pub use sqlite_errors::{map_sqlite_error, unique_constraint_error};
pub use tokenizer::{build_match_expression, tokenize};
pub use workspace_identity::{calculate_key, normalize_identifier};

#[cfg(test)]
mod tests {
    #[test]
    fn application_layer_builds() {
        assert!(super::tokenize("忆栈").contains("忆栈"));
    }
}
