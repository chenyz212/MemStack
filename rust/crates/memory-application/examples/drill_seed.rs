//! 升级/回滚演练辅助（第五轮 T9）：向指定数据库注入一个 MCP 客户端会话，
//! 输出 session_id 与明文令牌供演练脚本使用。
//!
//! 仅用于隔离副本（scripts/upgrade-rollback-drill.ps1）；生产库无 MCP 会话时，
//! 以此建立会话以验证 DPAPI 凭据链路与 stdio 鉴权。用法：
//! `cargo run --release -p memory-application --example drill_seed -- <db路径>`

use std::sync::Arc;

use memory_application::GuidGenerator;
use memory_application::clock::SystemClock;
use memory_application::db::Database;
use memory_application::mcp_access::McpAccessService;
use memory_domain::{CreateMcpClientRequest, McpPermission};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("用法：drill_seed <db路径>（应为隔离副本）");
    let path = std::path::PathBuf::from(path);
    let service = McpAccessService::new(Database::new(&path), Arc::new(SystemClock), Arc::new(GuidGenerator));
    let request = CreateMcpClientRequest {
        display_name: "演练客户端".to_string(),
        permission: McpPermission::ReadWrite,
        project_id: None,
        expires_at: None,
    };
    let secret = service.create_client(&request).expect("创建演练会话失败");
    println!("SESSION_ID={}", secret.client.session_id);
    println!("PLAIN_TOKEN={}", secret.plain_token);
}
