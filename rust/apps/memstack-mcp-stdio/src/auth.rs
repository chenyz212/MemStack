//! Token 鉴权：按 C# `McpAccessService.AuthenticateAsync` 语义加载客户端身份。
//!
//! 规则：SHA-256 小写哈希比对 `mcp_token.token_hash`，已吊销 Token 拒绝；
//! 过期 Token 拒绝。鉴权失败属于致命错误：进程以非 0 退出码结束。
//! 返回值直接采用 `memory_domain::McpCallerContext`，供 memory-mcp 分发层消费。
//!
//! 兼容治理（§7.3）：`MEMSTACK_TOKEN` 环境变量兼容路径保留至
//! **0.5.0 移除**；0.4.x 期间注册/预览只产出 `--session-id` 形态，桌面端
//! 对 env Token 旧配置显示一次性迁移提示（client_registration::check_path_health）。

use memory_domain::{McpCallerContext, McpPermission};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

/// 计算仅用于检索验证的 SHA-256 小写哈希（与 C# `HashToken` 一致）。
fn hash_token(plain_token: &str) -> String {
    let digest = Sha256::digest(plain_token.trim().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 校验 Token 过期时间；时间格式损坏时按无效 Token 拒绝，避免绕过过期限制。
fn validate_expiration(expires_at: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Result<(), String> {
    let Some(expires_at_text) = expires_at else {
        return Ok(());
    };
    let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at_text.trim())
        .map_err(|_| "MEMSTACK_TOKEN 无效或已过期".to_string())?;
    if expires_at <= now {
        return Err("MEMSTACK_TOKEN 无效或已过期".to_string());
    }
    Ok(())
}

/// 校验明文 Token 并加载身份；无效、吊销或过期时返回 Err（消息不含敏感信息）。
pub fn authenticate(connection: &Connection, plain_token: &str) -> Result<McpCallerContext, String> {
    if plain_token.trim().is_empty() {
        return Err("缺少 MEMSTACK_TOKEN 环境变量".to_string());
    }
    let hash = hash_token(plain_token);
    let row = connection
        .query_row(
            "SELECT id,display_name,permission,project_id,expires_at \
             FROM mcp_token WHERE token_hash=?1 AND revoked_at IS NULL;",
            [&hash],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .map_err(|_| "MEMSTACK_TOKEN 无效或已过期".to_string())?;
    let (token_id, display_name, permission_text, project_id, expires_at) = row;
    validate_expiration(expires_at.as_deref(), chrono::Utc::now())?;
    // permission 列为 "Read"/"ReadWrite"（flexible_enum 字符串形式）。
    let permission = serde_json::from_value::<McpPermission>(serde_json::Value::String(permission_text))
        .map_err(|_| "MEMSTACK_TOKEN 无效或已过期".to_string())?;
    Ok(McpCallerContext {
        token_id,
        display_name,
        permission,
        project_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_database() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute(
                "CREATE TABLE mcp_token (
                    id TEXT PRIMARY KEY,
                    token_hash TEXT NOT NULL UNIQUE,
                    display_name TEXT NULL,
                    permission TEXT NOT NULL,
                    project_id TEXT NULL,
                    expires_at TEXT NULL,
                    revoked_at TEXT NULL
                );",
                [],
            )
            .unwrap();
        let hash = hash_token("uam_valid_token");
        connection
            .execute(
                "INSERT INTO mcp_token(id,token_hash,display_name,permission,expires_at) VALUES('1',?1,'测试客户端','ReadWrite',NULL);",
                [&hash],
            )
            .unwrap();
        let revoked_hash = hash_token("uam_revoked_token");
        connection
            .execute(
                "INSERT INTO mcp_token(id,token_hash,permission,expires_at,revoked_at) \
                 VALUES('2',?1,'Read',NULL,'2026-01-01T00:00:00Z');",
                [&revoked_hash],
            )
            .unwrap();
        let expired_hash = hash_token("uam_expired_token");
        connection
            .execute(
                "INSERT INTO mcp_token(id,token_hash,permission,expires_at) \
                 VALUES('3',?1,'Read','2020-01-01T00:00:00+00:00');",
                [&expired_hash],
            )
            .unwrap();
        let expired_z_hash = hash_token("uam_expired_z_token");
        connection
            .execute(
                "INSERT INTO mcp_token(id,token_hash,permission,expires_at) \
                 VALUES('4',?1,'Read','2020-01-01T00:00:00Z');",
                [&expired_z_hash],
            )
            .unwrap();
        let malformed_expiration_hash = hash_token("uam_malformed_expiration_token");
        connection
            .execute(
                "INSERT INTO mcp_token(id,token_hash,permission,expires_at) \
                 VALUES('5',?1,'Read','not-a-time');",
                [&malformed_expiration_hash],
            )
            .unwrap();
        connection
    }

    #[test]
    fn valid_token_authenticates() {
        let connection = setup_database();
        let caller = authenticate(&connection, "uam_valid_token").unwrap();
        assert_eq!(caller.permission, McpPermission::ReadWrite);
        assert_eq!(caller.display_name, "测试客户端");
    }

    #[test]
    fn unknown_token_is_rejected() {
        let connection = setup_database();
        assert!(authenticate(&connection, "uam_unknown").is_err());
    }

    #[test]
    fn revoked_token_is_rejected() {
        let connection = setup_database();
        assert!(authenticate(&connection, "uam_revoked_token").is_err());
    }

    #[test]
    fn expired_token_is_rejected() {
        let connection = setup_database();
        assert!(authenticate(&connection, "uam_expired_token").is_err());
    }

    #[test]
    fn expired_z_token_is_rejected() {
        let connection = setup_database();
        assert!(authenticate(&connection, "uam_expired_z_token").is_err());
    }

    #[test]
    fn malformed_expiration_is_rejected() {
        let connection = setup_database();
        assert!(authenticate(&connection, "uam_malformed_expiration_token").is_err());
    }

    #[test]
    fn empty_token_is_rejected() {
        let connection = setup_database();
        assert!(authenticate(&connection, "  ").is_err());
    }
}
