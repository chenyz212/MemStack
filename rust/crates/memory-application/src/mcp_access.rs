//! MCP 访问控制：移植 C# `McpAccessService`（除连接测试）。
//!
//! - 静态校验（`require_*`）与 C# 逐字对齐。
//! - Token 三件套：`uam_` + 64 hex 明文 / SHA-256 小写哈希 / 前 12 字符前缀。
//! - DPAPI 密文经 `memory-platform::dpapi` 与 C# 双向兼容。
//! - 会话状态机 `compute_status` 由 revoked_at/expires_at 派生（注入 Clock，修复 C# 直用
//!   `DateTimeOffset.UtcNow` 的测试盲点，生产行为不变）。
//! - `TestClientAsync`（依赖 10212 HTTP 端点）本轮不迁移，属阶段 5（stdio 子进程实现）。

use std::sync::Arc;

use memory_domain::{
    BusinessError, ErrorCode, McpAssistantType, McpCallerContext, McpClientCard, McpClientSecret, McpClientStatus,
    McpPermission, McpTokenItem, McpTokenSecret, MemoryItem, MemoryScope,
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::sqlite_errors::{map_sqlite_error, unique_constraint_error};

/// Token 安全视图读取 SQL（与 C# `SelectTokenSql` 一致）。
const SELECT_TOKEN_SQL: &str = "
    SELECT t.id,t.assistant_type,t.display_name,t.token_prefix,t.permission,t.project_id,p.name, \
           t.expires_at,t.last_used_at,t.created_at,t.revoked_at \
    FROM mcp_token t LEFT JOIN project p ON p.id=t.project_id";

/// 动态客户端会话卡片读取 SQL（关联「最新 Token」：优先未吊销，再按创建时间倒序）。
const SELECT_CLIENT_SQL: &str = "
    SELECT s.id,s.client_key,s.display_name,s.client_version,s.transport, \
           t.token_prefix,t.permission,t.project_id,p.name, \
           t.expires_at,t.last_used_at,s.call_count,s.created_at,t.revoked_at \
    FROM mcp_client_session s \
    LEFT JOIN mcp_token t ON t.id = ( \
        SELECT t2.id FROM mcp_token t2 \
        WHERE t2.session_id = s.id \
        ORDER BY t2.revoked_at IS NULL DESC, t2.created_at DESC \
        LIMIT 1 \
    ) \
    LEFT JOIN project p ON p.id=t.project_id";

/// 验证调用者具备写权限（与 C# `RequireWrite` 一致）。
pub fn require_write(caller: &McpCallerContext) -> Result<(), BusinessError> {
    if caller.permission != McpPermission::ReadWrite {
        return Err(BusinessError::new(ErrorCode::McpPermissionDenied));
    }
    Ok(())
}

/// 验证请求范围是否位于 Token 绑定项目内（与 C# `RequireProjectScope` 一致）。
pub fn require_project_scope(
    caller: &McpCallerContext,
    scope: MemoryScope,
    project_id: Option<&str>,
) -> Result<(), BusinessError> {
    if let Some(bound) = caller.project_id.as_deref()
        && (scope != MemoryScope::Project || project_id != Some(bound))
    {
        return Err(BusinessError::new(ErrorCode::McpProjectScopeDenied));
    }
    Ok(())
}

/// 验证已有记忆是否位于 Token 允许范围内（与 C# `RequireMemoryScope` 一致）。
pub fn require_memory_scope(caller: &McpCallerContext, memory: &MemoryItem) -> Result<(), BusinessError> {
    require_project_scope(caller, memory.scope, memory.project_id.as_deref())
}

/// 生成 Token 明文：`uam_` + 64 位小写 hex（与 C# `CreatePlainToken` 一致）。
pub fn generate_plain_token() -> String {
    format!("uam_{}", hex_lower(&random_bytes_32()))
}

/// 计算 Token 哈希：SHA-256 小写 hex（与 C# `HashToken` 一致）。
pub fn hash_token(plain_token: &str) -> String {
    hex_lower(&Sha256::digest(plain_token.as_bytes()))
}

/// Token 前缀：前 12 个字符（与 C# `CreatePrefix` 一致）。
pub fn token_prefix(plain_token: &str) -> String {
    plain_token.chars().take(12).collect()
}

/// 根据吊销与过期时间推断卡片状态（与 C# `ComputeStatus` 一致，注入 Clock）。
pub fn compute_status(revoked_at: Option<&str>, expires_at: Option<&str>, now_text: &str) -> McpClientStatus {
    if revoked_at.is_some() {
        return McpClientStatus::Revoked;
    }
    if let Some(expires) = expires_at
        && !expires.is_empty()
        && expires <= now_text
    {
        return McpClientStatus::Expired;
    }
    McpClientStatus::Active
}

/// 名称规范化：去首尾空格、转小写、移除空白字符（与 C# `NormalizeClientKey` 一致）。
pub fn normalize_client_key(display_name: &str) -> String {
    display_name
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn random_bytes_32() -> [u8; 32] {
    // uuid v4 两枚拼接提供 256 位熵（uuid 内部 CSPRNG）。
    let first = uuid::Uuid::new_v4();
    let second = uuid::Uuid::new_v4();
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(first.as_bytes());
    bytes[16..].copy_from_slice(second.as_bytes());
    bytes
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn parse_permission(value: &str) -> McpPermission {
    if value.eq_ignore_ascii_case("ReadWrite") {
        McpPermission::ReadWrite
    } else {
        McpPermission::Read
    }
}

fn parse_assistant_type(value: &str) -> McpAssistantType {
    match value.to_lowercase().as_str() {
        "codex" => McpAssistantType::Codex,
        "claude" => McpAssistantType::Claude,
        "cursor" => McpAssistantType::Cursor,
        "trae" => McpAssistantType::Trae,
        _ => McpAssistantType::Generic,
    }
}

fn permission_text(permission: McpPermission) -> &'static str {
    match permission {
        McpPermission::Read => "Read",
        McpPermission::ReadWrite => "ReadWrite",
    }
}

/// MCP 最近记忆活动记录器（记录成功的记忆读取与写入操作）。
///
/// 写入失败静默（活动文案是展示增强，不阻断工具响应）。
pub struct MemoryActivityRecorder {
    database: Database,
    clock: Arc<dyn Clock>,
}

impl MemoryActivityRecorder {
    /// 创建活动记录器。
    pub fn new(database: Database, clock: Arc<dyn Clock>) -> Self {
        Self { database, clock }
    }

    /// 记录一次成功的记忆活动。
    ///
    /// - `action`：`READ` / `CREATE` / `UPDATE` / `ARCHIVE`。
    /// - `scope`：`Personal` / `Project` / `Mixed`。
    pub fn record(&self, token_id: &str, action: &str, scope: &str) {
        let Ok(connection) = self.database.open() else {
            return;
        };
        let now_text = format_storage_time(self.clock.now_utc());
        let _ = connection.execute(
            "UPDATE mcp_token SET last_memory_action=$action,last_action_scope=$scope,last_action_at=$at WHERE id=$id;",
            params![action, scope, now_text, token_id],
        );
    }
}

/// 管理使用 DPAPI 加密保存的独立 MCP 访问令牌。
pub struct McpAccessService {
    database: Database,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl McpAccessService {
    /// 创建 MCP 访问服务。
    pub fn new(database: Database, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self { database, clock, ids }
    }

    // ===== 兼容 Token API =====

    /// 返回全部 AI 助手令牌的安全元数据。
    pub fn list(&self) -> Result<Vec<McpTokenItem>, BusinessError> {
        let connection = self.database.open()?;
        let sql = format!("{SELECT_TOKEN_SQL} ORDER BY t.created_at DESC,t.id DESC;");
        let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
        let rows = statement
            .query_map([], read_token)
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(rows)
    }

    /// 为一个 AI 助手签发新的可恢复令牌。
    pub fn create_token(
        &self,
        request: &memory_domain::CreateMcpTokenRequest,
    ) -> Result<McpTokenSecret, BusinessError> {
        validate_token_request(request)?;
        let connection = self.database.open()?;
        validate_token_project(&connection, request.project_id.as_deref())?;
        let id = self.ids.new_id();
        let plain_token = generate_plain_token();
        let now_text = format_storage_time(self.clock.now_utc());
        let ciphertext = memory_platform::dpapi::protect(&plain_token)
            .map_err(|_| BusinessError::new(ErrorCode::McpTokenDecryptFailed))?;
        connection
            .execute(
                "INSERT INTO mcp_token( \
                     id,name,token_hash,access_mode,project_scope_json,expires_at,revoked_at,created_at, \
                     assistant_type,display_name,token_prefix,token_ciphertext,permission,project_id,last_used_at) \
                 VALUES( \
                     $id,$name,$token_hash,$access_mode,$project_scope_json,$expires_at,NULL,$created_at, \
                     $assistant_type,$display_name,$token_prefix,$token_ciphertext,$permission,$project_id,NULL);",
                params![
                    id,
                    request.display_name.trim(),
                    hash_token(&plain_token),
                    permission_text(request.permission),
                    project_scope_json(request.project_id.as_deref()),
                    request.expires_at,
                    now_text,
                    assistant_type_text(request.assistant_type),
                    request.display_name.trim(),
                    token_prefix(&plain_token),
                    ciphertext,
                    permission_text(request.permission),
                    request.project_id,
                ],
            )
            .map_err(map_sqlite_error)?;
        let item = get_token_item(&connection, &id)?;
        Ok(McpTokenSecret {
            token: item,
            plain_token,
        })
    }

    /// 解密活动令牌，供桌面页面按需显示或复制。
    pub fn get_secret(&self, id: &str) -> Result<McpTokenSecret, BusinessError> {
        let connection = self.database.open()?;
        let item = get_token_item(&connection, id)?;
        if item.revoked_at.is_some() {
            return Err(BusinessError::new(ErrorCode::McpTokenRevoked));
        }
        decrypt_token(&connection, id, item)
    }

    /// 吊销令牌并立即清除可恢复密文。
    pub fn revoke(&self, id: &str) -> Result<McpTokenItem, BusinessError> {
        let connection = self.database.open()?;
        let now_text = format_storage_time(self.clock.now_utc());
        let changed = connection
            .execute(
                "UPDATE mcp_token SET revoked_at=$time,token_ciphertext='' WHERE id=$id AND revoked_at IS NULL;",
                params![now_text, id],
            )
            .map_err(map_sqlite_error)?;
        if changed == 0 {
            // 不存在或已吊销：不存在时抛 NOT_FOUND，已吊销时返回当前状态。
            get_token_item(&connection, id)?;
        }
        get_token_item(&connection, id)
    }

    /// 为同一助手重新生成令牌并恢复活动状态。
    pub fn regenerate(&self, id: &str) -> Result<McpTokenSecret, BusinessError> {
        let plain_token = generate_plain_token();
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        get_token_item(&connection, id)?;
        let ciphertext = memory_platform::dpapi::protect(&plain_token)
            .map_err(|_| BusinessError::new(ErrorCode::McpTokenDecryptFailed))?;
        connection
            .execute(
                "UPDATE mcp_token \
                 SET token_hash=$hash,token_prefix=$prefix,token_ciphertext=$ciphertext, \
                     revoked_at=NULL,last_used_at=NULL,created_at=$created_at \
                 WHERE id=$id;",
                params![
                    hash_token(&plain_token),
                    token_prefix(&plain_token),
                    ciphertext,
                    now_text,
                    id
                ],
            )
            .map_err(map_sqlite_error)?;
        let item = get_token_item(&connection, id)?;
        Ok(McpTokenSecret {
            token: item,
            plain_token,
        })
    }

    /// 验证明文 Token、有效期与吊销状态；无效返回 `None`（与 C# null 语义一致）。
    pub fn authenticate(&self, plain_token: &str) -> Result<Option<McpCallerContext>, BusinessError> {
        if plain_token.trim().is_empty() {
            return Ok(None);
        }
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        let row: Option<AuthRow> = connection
            .query_row(
                "SELECT id,display_name,permission,project_id,expires_at,session_id \
                     FROM mcp_token WHERE token_hash=$hash AND revoked_at IS NULL;",
                params![hash_token(plain_token.trim())],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        let Some((id, display_name, permission_text_value, project_id, expires_at, session_id)) = row else {
            return Ok(None);
        };
        if let Some(expires) = expires_at.as_deref()
            && !expires.is_empty()
            && expires <= now_text.as_str()
        {
            return Ok(None);
        }
        touch(&connection, &id, session_id.as_deref(), &now_text)?;
        Ok(Some(McpCallerContext {
            token_id: id,
            display_name,
            permission: parse_permission(&permission_text_value),
            project_id,
        }))
    }

    /// 兼容请求管道并验证 Bearer Token（与 C# `ValidateAsync` 一致）。
    pub fn validate(&self, authorization: Option<&str>) -> Result<bool, BusinessError> {
        let Some(authorization) = authorization else {
            return Ok(false);
        };
        let Some(token) = authorization
            .strip_prefix("Bearer ")
            .or_else(|| authorization.strip_prefix("bearer "))
        else {
            return Ok(false);
        };
        Ok(self.authenticate(token.trim())?.is_some())
    }

    // ===== 动态客户端会话 API =====

    /// 列出全部动态客户端会话的安全卡片视图。
    pub fn list_clients(&self) -> Result<Vec<McpClientCard>, BusinessError> {
        let connection = self.database.open()?;
        let now_text = format_storage_time(self.clock.now_utc());
        let sql = format!("{SELECT_CLIENT_SQL} ORDER BY s.created_at DESC, s.id DESC;");
        let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
        let rows = statement
            .query_map([], |row| read_client(row, &now_text))
            .map_err(map_sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite_error)?;
        Ok(rows)
    }

    /// 读取指定动态客户端会话的安全卡片视图。
    pub fn get_client(&self, session_id: &str) -> Result<McpClientCard, BusinessError> {
        let connection = self.database.open()?;
        let now_text = format_storage_time(self.clock.now_utc());
        get_client_card(&connection, session_id, &now_text)
    }

    /// 创建一个新的 AI 工具：事务中建立会话和首个 Token。
    pub fn create_client(
        &self,
        request: &memory_domain::CreateMcpClientRequest,
    ) -> Result<McpClientSecret, BusinessError> {
        validate_client_request(&request.display_name, request.permission)?;
        let mut connection = self.database.open()?;
        validate_token_project(&connection, request.project_id.as_deref())?;
        let display_name = request.display_name.trim().to_string();
        let client_key = normalize_client_key(&display_name);
        let session_id = self.ids.new_id();
        let token_id = self.ids.new_id();
        let plain_token = generate_plain_token();
        let now_text = format_storage_time(self.clock.now_utc());
        let expires_at_text = request.expires_at.clone();

        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        transaction
            .execute(
                "INSERT INTO mcp_client_session(id, client_key, display_name, client_version, transport, \
                 last_seen_at, call_count, created_at, updated_at) \
                 VALUES($id, $client_key, $display_name, NULL, 'http', NULL, 0, $created_at, $updated_at);",
                params![session_id, client_key, display_name, now_text, now_text],
            )
            .map_err(|error| duplicate_client_error(error, map_sqlite_error))?;
        insert_client_token(
            &transaction,
            &token_id,
            &session_id,
            &display_name,
            request.permission,
            request.project_id.as_deref(),
            expires_at_text.as_deref(),
            &plain_token,
            &now_text,
        )?;
        transaction.commit().map_err(map_sqlite_error)?;
        let client = get_client_card(&connection, &session_id, &now_text)?;
        Ok(McpClientSecret { client, plain_token })
    }

    /// 编辑已有 AI 工具的名称、权限、项目范围与有效期，不改变 Token 明文。
    pub fn update_client(
        &self,
        session_id: &str,
        request: &memory_domain::UpdateMcpClientRequest,
    ) -> Result<McpClientCard, BusinessError> {
        validate_client_request(&request.display_name, request.permission)?;
        let mut connection = self.database.open()?;
        validate_token_project(&connection, request.project_id.as_deref())?;
        let existing = self.get_client(session_id)?;
        let new_display_name = request.display_name.trim().to_string();
        let new_client_key = normalize_client_key(&new_display_name);
        if new_client_key != existing.client_key {
            let conflict: i64 = connection
                .query_row(
                    "SELECT count(*) FROM mcp_client_session WHERE client_key=$key AND id<>$id;",
                    params![new_client_key, session_id],
                    |row| row.get(0),
                )
                .map_err(map_sqlite_error)?;
            if conflict > 0 {
                return Err(BusinessError::new(ErrorCode::McpClientDuplicate));
            }
        }
        let now_text = format_storage_time(self.clock.now_utc());
        let expires_at_text = if request.clear_expires_at {
            None
        } else {
            request.expires_at.clone()
        };
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        transaction
            .execute(
                "UPDATE mcp_client_session \
                 SET client_key=$client_key, display_name=$display_name, updated_at=$updated_at \
                 WHERE id=$id;",
                params![new_client_key, new_display_name, now_text, session_id],
            )
            .map_err(|error| duplicate_client_error(error, map_sqlite_error))?;
        if !existing.token_prefix.is_empty() {
            transaction
                .execute(
                    "UPDATE mcp_token \
                     SET display_name=$display_name, permission=$permission, project_id=$project_id, \
                         project_scope_json=$project_scope_json, expires_at=$expires_at \
                     WHERE session_id=$session_id AND revoked_at IS NULL;",
                    params![
                        new_display_name,
                        permission_text(request.permission),
                        request.project_id,
                        project_scope_json(request.project_id.as_deref()),
                        expires_at_text,
                        session_id,
                    ],
                )
                .map_err(map_sqlite_error)?;
        }
        transaction.commit().map_err(map_sqlite_error)?;
        self.get_client(session_id)
    }

    /// 轮换客户端会话令牌：旧 Token 立即吊销，生成新 Token，继续绑定原会话。
    pub fn rotate_client(&self, session_id: &str) -> Result<McpClientSecret, BusinessError> {
        let plain_token = generate_plain_token();
        let now_text = format_storage_time(self.clock.now_utc());
        let mut connection = self.database.open()?;
        let existing = self.get_client(session_id)?;
        let token_id = self.ids.new_id();
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        transaction
            .execute(
                "UPDATE mcp_token SET revoked_at=$time, token_ciphertext='' \
                 WHERE session_id=$session_id AND revoked_at IS NULL;",
                params![now_text, session_id],
            )
            .map_err(map_sqlite_error)?;
        insert_client_token(
            &transaction,
            &token_id,
            session_id,
            &existing.display_name,
            existing.permission,
            existing.project_id.as_deref(),
            existing.expires_at.as_deref(),
            &plain_token,
            &now_text,
        )?;
        transaction
            .execute(
                "UPDATE mcp_client_session SET updated_at=$updated_at WHERE id=$id;",
                params![now_text, session_id],
            )
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)?;
        let client = get_client_card(&connection, session_id, &now_text)?;
        Ok(McpClientSecret { client, plain_token })
    }

    /// 轮换会话 ID：生成新 GUID 替换 `mcp_client_session` 主键，Token 归属一并迁移。
    ///
    /// 旧 `--session-id` 立即失效（会话不存在 → stdio 认证失败）；会话属性
    /// （名称/权限/项目/调用统计）与令牌保持不变，无需重新生成令牌。
    /// 返回新会话卡片；调用方需提示用户重新写入 AI 客户端接入配置。
    pub fn rotate_client_session_id(&self, session_id: &str) -> Result<McpClientCard, BusinessError> {
        let new_session_id = self.ids.new_id();
        let now_text = format_storage_time(self.clock.now_utc());
        let mut connection = self.database.open()?;
        // 会话存在即可轮换（吊销态卡片同样可换 id，语义与 rotate_client 一致）。
        let existing: Option<String> = connection
            .query_row(
                "SELECT id FROM mcp_client_session WHERE id=$id;",
                params![session_id],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        if existing.is_none() {
            return Err(BusinessError::new(ErrorCode::McpClientNotFound));
        }
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        // FK 即时检查会拦下两条 UPDATE 的中间态（token 仍指旧 id / 新 id 尚无父行），
        // 推迟到事务提交时统一校验。
        transaction
            .execute("PRAGMA defer_foreign_keys=true;", [])
            .map_err(map_sqlite_error)?;
        transaction
            .execute(
                "UPDATE mcp_client_session SET id=$new_id, updated_at=$updated_at WHERE id=$old_id;",
                params![new_session_id, now_text, session_id],
            )
            .map_err(map_sqlite_error)?;
        transaction
            .execute(
                "UPDATE mcp_token SET session_id=$new_id WHERE session_id=$old_id;",
                params![new_session_id, session_id],
            )
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)?;
        get_client_card(&connection, &new_session_id, &now_text)
    }

    /// 吊销客户端会话当前令牌：保留客户端卡片，状态变为「未启用」，允许重新生成。
    pub fn revoke_client(&self, session_id: &str) -> Result<McpClientCard, BusinessError> {
        let connection = self.database.open()?;
        get_client_card(&connection, session_id, &format_storage_time(self.clock.now_utc()))?;
        let now_text = format_storage_time(self.clock.now_utc());
        connection
            .execute(
                "UPDATE mcp_token SET revoked_at=$time, token_ciphertext='' \
                 WHERE session_id=$session_id AND revoked_at IS NULL;",
                params![now_text, session_id],
            )
            .map_err(map_sqlite_error)?;
        self.get_client(session_id)
    }

    /// 删除客户端会话：幂等移除关联的全部 Token 与会话记录本身。
    pub fn delete_client(&self, session_id: &str) -> Result<(), BusinessError> {
        let mut connection = self.database.open()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        transaction
            .execute(
                "DELETE FROM mcp_token WHERE session_id=$session_id;",
                params![session_id],
            )
            .map_err(map_sqlite_error)?;
        transaction
            .execute("DELETE FROM mcp_client_session WHERE id=$id;", params![session_id])
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 解密客户端会话当前活动令牌，供桌面页面按需显示或复制。
    pub fn get_client_secret(&self, session_id: &str) -> Result<McpClientSecret, BusinessError> {
        let connection = self.database.open()?;
        let client = self.get_client(session_id)?;
        if client.status == McpClientStatus::Revoked {
            return Err(BusinessError::new(ErrorCode::McpTokenRevoked));
        }
        let ciphertext: Option<String> = connection
            .query_row(
                "SELECT token_ciphertext FROM mcp_token \
                 WHERE session_id=$session_id AND revoked_at IS NULL \
                 ORDER BY created_at DESC LIMIT 1;",
                params![session_id],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        let ciphertext = ciphertext.unwrap_or_default();
        if ciphertext.is_empty() {
            return Err(BusinessError::new(ErrorCode::McpTokenRegenerateRequired));
        }
        let plain_token = memory_platform::dpapi::unprotect(&ciphertext)
            .map_err(|_| BusinessError::new(ErrorCode::McpTokenDecryptFailed))?;
        Ok(McpClientSecret { client, plain_token })
    }
}

/// 更新最近调用时间，并同步动态客户端会话的调用次数与最近调用时间（C# `TouchAsync`）。
fn touch(connection: &Connection, id: &str, session_id: Option<&str>, now_text: &str) -> Result<(), BusinessError> {
    connection
        .execute(
            "UPDATE mcp_token SET last_used_at=$time WHERE id=$id;",
            params![now_text, id],
        )
        .map_err(map_sqlite_error)?;
    if let Some(session) = session_id {
        connection
            .execute(
                "UPDATE mcp_client_session \
                 SET last_seen_at=$time, call_count=call_count+1, updated_at=$time \
                 WHERE id=$id;",
                params![now_text, session],
            )
            .map_err(map_sqlite_error)?;
    }
    Ok(())
}

/// 读取指定 Token 安全元数据（不存在抛 `MCP_TOKEN_NOT_FOUND`）。
fn get_token_item(connection: &Connection, id: &str) -> Result<McpTokenItem, BusinessError> {
    let sql = format!("{SELECT_TOKEN_SQL} WHERE t.id=$id;");
    connection
        .query_row(&sql, params![id], read_token)
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => BusinessError::new(ErrorCode::McpTokenNotFound),
            other => map_sqlite_error(other),
        })
}

/// 从行构造 Token 安全视图（与 C# `ReadToken` 一致）。
fn read_token(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpTokenItem> {
    let assistant_text: String = row.get(1)?;
    let permission_text_value: String = row.get(4)?;
    Ok(McpTokenItem {
        id: row.get(0)?,
        assistant_type: parse_assistant_type(&assistant_text),
        display_name: row.get(2)?,
        token_prefix: row.get(3)?,
        permission: parse_permission(&permission_text_value),
        project_id: row.get(5)?,
        project_name: row.get(6)?,
        expires_at: row.get(7)?,
        last_used_at: row.get(8)?,
        created_at: row.get(9)?,
        revoked_at: row.get(10)?,
    })
}

/// 读取指定动态客户端会话卡片（不存在抛 `MCP_CLIENT_NOT_FOUND`）。
fn get_client_card(connection: &Connection, session_id: &str, now_text: &str) -> Result<McpClientCard, BusinessError> {
    let sql = format!("{SELECT_CLIENT_SQL} WHERE s.id=$id;");
    connection
        .query_row(&sql, params![session_id], |row| read_client(row, now_text))
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => BusinessError::new(ErrorCode::McpClientNotFound),
            other => map_sqlite_error(other),
        })
}

/// 从行构造动态客户端会话安全视图（与 C# `ReadClient` 一致；transport 缺省 http）。
fn read_client(row: &rusqlite::Row<'_>, now_text: &str) -> rusqlite::Result<McpClientCard> {
    let transport: Option<String> = row.get(4)?;
    let token_prefix: Option<String> = row.get(5)?;
    let permission: Option<String> = row.get(6)?;
    let expires_at: Option<String> = row.get(9)?;
    let revoked_at: Option<String> = row.get(13)?;
    let permission = match permission.as_deref() {
        Some(value) => parse_permission(value),
        None => McpPermission::Read,
    };
    let status = compute_status(revoked_at.as_deref(), expires_at.as_deref(), now_text);
    Ok(McpClientCard {
        session_id: row.get(0)?,
        client_key: row.get(1)?,
        display_name: row.get(2)?,
        client_version: row.get(3)?,
        transport: transport.unwrap_or_else(|| "http".to_string()),
        token_prefix: token_prefix.unwrap_or_default(),
        permission,
        project_id: row.get(7)?,
        project_name: row.get(8)?,
        expires_at,
        last_used_at: row.get(10)?,
        call_count: row.get(11)?,
        created_at: row.get(12)?,
        revoked_at,
        status,
    })
}

/// 认证查询行：id、显示名、权限、项目、过期时间、所属会话。
type AuthRow = (String, String, String, Option<String>, Option<String>, Option<String>);

/// 写入动态客户端会话 Token 的全部字段（与 C# `AddClientTokenParameters` 参数一一对应）。
#[allow(clippy::too_many_arguments)]
fn insert_client_token(
    transaction: &rusqlite::Transaction<'_>,
    token_id: &str,
    session_id: &str,
    display_name: &str,
    permission: McpPermission,
    project_id: Option<&str>,
    expires_at_text: Option<&str>,
    plain_token: &str,
    now_text: &str,
) -> Result<(), BusinessError> {
    let ciphertext = memory_platform::dpapi::protect(plain_token)
        .map_err(|_| BusinessError::new(ErrorCode::McpTokenDecryptFailed))?;
    transaction
        .execute(
            "INSERT INTO mcp_token( \
                 id,name,token_hash,access_mode,project_scope_json,expires_at,revoked_at,created_at, \
                 assistant_type,display_name,token_prefix,token_ciphertext,permission,project_id,last_used_at,session_id) \
             VALUES( \
                 $id,$name,$token_hash,$access_mode,$project_scope_json,$expires_at,NULL,$created_at, \
                 $assistant_type,$display_name,$token_prefix,$token_ciphertext,$permission,$project_id,NULL,$session_id);",
            params![
                token_id,
                display_name,
                hash_token(plain_token),
                permission_text(permission),
                project_scope_json(project_id),
                expires_at_text,
                now_text,
                assistant_type_text(McpAssistantType::Generic),
                display_name,
                token_prefix(plain_token),
                ciphertext,
                permission_text(permission),
                project_id,
                session_id,
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

/// 解密指定 Token 密文并组装完整视图（与 C# `GetSecretAsync` 一致）。
fn decrypt_token(connection: &Connection, id: &str, item: McpTokenItem) -> Result<McpTokenSecret, BusinessError> {
    let ciphertext: String = connection
        .query_row(
            "SELECT token_ciphertext FROM mcp_token WHERE id=$id;",
            params![id],
            |row| row.get(0),
        )
        .unwrap_or_default();
    if ciphertext.is_empty() {
        return Err(BusinessError::new(ErrorCode::McpTokenRegenerateRequired));
    }
    let plain_token = memory_platform::dpapi::unprotect(&ciphertext)
        .map_err(|_| BusinessError::new(ErrorCode::McpTokenDecryptFailed))?;
    Ok(McpTokenSecret {
        token: item,
        plain_token,
    })
}

/// 校验签发请求的全部业务字段（与 C# `ValidateRequest` 一致）。
fn validate_token_request(request: &memory_domain::CreateMcpTokenRequest) -> Result<(), BusinessError> {
    let utf16_length = |value: &str| value.chars().map(|c| c.len_utf16()).sum::<usize>();
    if request.display_name.trim().is_empty() || utf16_length(request.display_name.trim()) > 80 {
        return Err(BusinessError::with_message(
            ErrorCode::McpDisplayNameInvalid,
            "AI 助手名称长度必须为 1 到 80 个字符",
        ));
    }
    Ok(())
}

/// 校验动态客户端会话请求的字段（与 C# `ValidateClientRequest` 一致）。
fn validate_client_request(display_name: &str, _permission: McpPermission) -> Result<(), BusinessError> {
    let utf16_length = |value: &str| value.chars().map(|c| c.len_utf16()).sum::<usize>();
    if display_name.trim().is_empty() || utf16_length(display_name.trim()) > 80 {
        return Err(BusinessError::with_message(
            ErrorCode::McpDisplayNameInvalid,
            "AI 工具名称长度必须为 1 到 80 个字符",
        ));
    }
    Ok(())
}

/// 校验项目范围指向活动项目（与 C# `ValidateProjectAsync` 一致）。
fn validate_token_project(connection: &Connection, project_id: Option<&str>) -> Result<(), BusinessError> {
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let archived: Option<i64> = connection
        .query_row(
            "SELECT is_archived FROM project WHERE id=$id;",
            params![project_id],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(map_sqlite_error)?;
    match archived {
        None => Err(BusinessError::new(ErrorCode::ProjectNotFound)),
        Some(value) if value != 0 => Err(BusinessError::with_message(
            ErrorCode::ProjectArchived,
            "归档项目不能用于 MCP Token 范围",
        )),
        Some(_) => Ok(()),
    }
}

/// 项目范围 JSON：绑定项目为 `["<guid>"]`，全局为 `[]`。
fn project_scope_json(project_id: Option<&str>) -> String {
    match project_id {
        Some(id) => format!("[\"{id}\"]"),
        None => "[]".to_string(),
    }
}

fn assistant_type_text(assistant_type: McpAssistantType) -> &'static str {
    match assistant_type {
        McpAssistantType::Codex => "Codex",
        McpAssistantType::Claude => "Claude",
        McpAssistantType::Cursor => "Cursor",
        McpAssistantType::Trae => "Trae",
        McpAssistantType::Generic => "Generic",
    }
}

/// 唯一约束冲突 → `MCP_CLIENT_DUPLICATE`（会话名唯一索引兜底）。
fn duplicate_client_error(error: rusqlite::Error, fallback: fn(rusqlite::Error) -> BusinessError) -> BusinessError {
    if unique_constraint_error(&error).is_some() {
        return BusinessError::new(ErrorCode::McpClientDuplicate);
    }
    fallback(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use chrono::{TimeZone, Utc};
    use memory_domain::CreateMcpClientRequest;

    struct TestContext {
        service: McpAccessService,
        database: Database,
        _temp: tempfile::TempDir,
    }

    fn context() -> TestContext {
        let temp = tempfile::tempdir().unwrap();
        drop(memory_storage::open_initialized(&temp.path().join("test.db")).unwrap());
        let database = Database::new(temp.path().join("test.db"));
        let clock = Arc::new(FixedClock::new(Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap()));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=20)
                .map(|index| format!("{index:08}-{index:04}-4{index:03}-8{index:03}-{index:012}"))
                .collect(),
        ));
        let service = McpAccessService::new(database.clone(), clock, ids);
        TestContext {
            service,
            database,
            _temp: temp,
        }
    }

    fn token_request(display_name: &str) -> memory_domain::CreateMcpTokenRequest {
        memory_domain::CreateMcpTokenRequest {
            assistant_type: McpAssistantType::Codex,
            display_name: display_name.to_string(),
            permission: McpPermission::ReadWrite,
            project_id: None,
            expires_at: None,
        }
    }

    fn client_request(display_name: &str) -> CreateMcpClientRequest {
        CreateMcpClientRequest {
            display_name: display_name.to_string(),
            permission: McpPermission::ReadWrite,
            project_id: None,
            expires_at: None,
        }
    }

    #[test]
    fn token_trio_matches_csharp_shape() {
        let plain = generate_plain_token();
        assert!(plain.starts_with("uam_"));
        assert_eq!(plain.len(), 4 + 64);
        let hex_part = &plain[4..];
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(hex_part.chars().all(|c| !c.is_ascii_uppercase()));
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(token_prefix(&plain), plain.chars().take(12).collect::<String>());
    }

    #[test]
    fn require_write_and_scope_semantics() {
        let reader = McpCallerContext {
            token_id: "t1".to_string(),
            display_name: "只读".to_string(),
            permission: McpPermission::Read,
            project_id: None,
        };
        let error = require_write(&reader).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpPermissionDenied);
        assert_eq!(error.message, "当前 Token 仅有读取权限");

        let scoped = McpCallerContext {
            token_id: "t2".to_string(),
            display_name: "项目".to_string(),
            permission: McpPermission::ReadWrite,
            project_id: Some("p1".to_string()),
        };
        require_write(&scoped).unwrap();
        require_project_scope(&scoped, MemoryScope::Project, Some("p1")).unwrap();
        let error = require_project_scope(&scoped, MemoryScope::Personal, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpProjectScopeDenied);
        // 全局 Token 不受限。
        let global = McpCallerContext {
            project_id: None,
            ..scoped.clone()
        };
        require_project_scope(&global, MemoryScope::Personal, None).unwrap();
    }

    #[test]
    fn token_lifecycle_create_secret_revoke_regenerate() {
        let context = context();
        let secret = context.service.create_token(&token_request("Codex 助手")).unwrap();
        assert!(secret.plain_token.starts_with("uam_"));
        assert_eq!(secret.token.display_name, "Codex 助手");
        assert_eq!(secret.token.revoked_at, None);

        // 明文可鉴权。
        let caller = context.service.authenticate(&secret.plain_token).unwrap().unwrap();
        assert_eq!(caller.display_name, "Codex 助手");
        // 鉴权更新 last_used_at。
        let item = context
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT last_used_at FROM mcp_token WHERE id=$1;",
                params![caller.token_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap();
        assert!(item.is_some());

        // 密文可解密回明文（DPAPI 往返）。
        let revealed = context.service.get_secret(&secret.token.id).unwrap();
        assert_eq!(revealed.plain_token, secret.plain_token);

        // 吊销后：密文清空、明文失效、解密拒绝。
        let revoked = context.service.revoke(&secret.token.id).unwrap();
        assert!(revoked.revoked_at.is_some());
        assert!(context.service.authenticate(&secret.plain_token).unwrap().is_none());
        let error = context.service.get_secret(&secret.token.id).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpTokenRevoked);

        // 重新生成：恢复活动且新明文可用。
        let regenerated = context.service.regenerate(&secret.token.id).unwrap();
        assert!(regenerated.token.revoked_at.is_none());
        assert_ne!(regenerated.plain_token, secret.plain_token);
        assert!(
            context
                .service
                .authenticate(&regenerated.plain_token)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn invalid_or_unknown_tokens_fail_authentication() {
        let context = context();
        assert!(context.service.authenticate("").unwrap().is_none());
        assert!(context.service.authenticate("   ").unwrap().is_none());
        let unknown = format!("uam_{}", "0".repeat(64));
        assert!(context.service.authenticate(&unknown).unwrap().is_none());
        // Bearer 校验。
        assert!(!context.service.validate(None).unwrap());
        assert!(!context.service.validate(Some("Basic abc")).unwrap());
        assert!(!context.service.validate(Some(&format!("Bearer {unknown}"))).unwrap());
    }

    #[test]
    fn client_lifecycle_create_update_rotate_revoke_delete() {
        let context = context();
        let secret = context.service.create_client(&client_request("Claude 桌面")).unwrap();
        assert_eq!(secret.client.display_name, "Claude 桌面");
        assert_eq!(secret.client.status, McpClientStatus::Active);
        assert_eq!(secret.client.transport, "http");
        assert_eq!(secret.client.call_count, 0);
        assert!(!secret.plain_token.is_empty());

        // 同名（规范化后）冲突。
        let error = context
            .service
            .create_client(&client_request("claude  桌面 "))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::McpClientDuplicate);

        // 明文可鉴权且累计调用次数。
        context.service.authenticate(&secret.plain_token).unwrap().unwrap();
        let card = context.service.get_client(&secret.client.session_id).unwrap();
        assert_eq!(card.call_count, 1);
        assert!(card.last_used_at.is_some());

        // 编辑：改名 + 权限。
        let updated = context
            .service
            .update_client(
                &secret.client.session_id,
                &memory_domain::UpdateMcpClientRequest {
                    display_name: "Claude 重命名".to_string(),
                    permission: McpPermission::Read,
                    project_id: None,
                    expires_at: None,
                    clear_expires_at: false,
                },
            )
            .unwrap();
        assert_eq!(updated.display_name, "Claude 重命名");
        assert_eq!(updated.permission, McpPermission::Read);
        // 改名冲突：目标名已被占用。
        let second = context.service.create_client(&client_request("另一个工具")).unwrap();
        let error = context
            .service
            .update_client(
                &second.client.session_id,
                &memory_domain::UpdateMcpClientRequest {
                    display_name: "Claude 重命名".to_string(),
                    permission: McpPermission::Read,
                    project_id: None,
                    expires_at: None,
                    clear_expires_at: false,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::McpClientDuplicate);

        // 轮换：旧明文失效、新明文生效、卡片仍 Active。
        let rotated = context.service.rotate_client(&secret.client.session_id).unwrap();
        assert_eq!(rotated.client.status, McpClientStatus::Active);
        assert!(context.service.authenticate(&secret.plain_token).unwrap().is_none());
        assert!(context.service.authenticate(&rotated.plain_token).unwrap().is_some());

        // 吊销：状态 Revoked，密文不可解。
        let revoked = context.service.revoke_client(&secret.client.session_id).unwrap();
        assert_eq!(revoked.status, McpClientStatus::Revoked);
        let error = context
            .service
            .get_client_secret(&secret.client.session_id)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::McpTokenRevoked);

        // 删除：幂等清理。
        context.service.delete_client(&secret.client.session_id).unwrap();
        let error = context.service.get_client(&secret.client.session_id).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpClientNotFound);
        // 再次删除不报错。
        context.service.delete_client(&secret.client.session_id).unwrap();
    }

    #[test]
    fn client_secret_reveals_current_token() {
        let context = context();
        let secret = context.service.create_client(&client_request("Cursor")).unwrap();
        let revealed = context.service.get_client_secret(&secret.client.session_id).unwrap();
        assert_eq!(revealed.plain_token, secret.plain_token);
        // 轮换后旧 secret 失效、新的可解。
        let rotated = context.service.rotate_client(&secret.client.session_id).unwrap();
        let revealed2 = context.service.get_client_secret(&secret.client.session_id).unwrap();
        assert_eq!(revealed2.plain_token, rotated.plain_token);
    }

    #[test]
    fn rotate_session_id_moves_tokens_and_invalidates_old_id() {
        let context = context();
        let secret = context.service.create_client(&client_request("Codex")).unwrap();
        let old_session_id = secret.client.session_id.clone();

        // 先产生一次调用统计（便于验证属性保留）。
        context.service.authenticate(&secret.plain_token).unwrap().unwrap();

        let card = context.service.rotate_client_session_id(&old_session_id).unwrap();
        assert_ne!(card.session_id, old_session_id);
        assert_eq!(card.display_name, "Codex");
        assert_eq!(card.permission, McpPermission::ReadWrite);
        assert_eq!(card.call_count, 1, "调用统计随会话保留");
        assert_eq!(card.token_prefix, secret.client.token_prefix, "令牌不随会话 ID 轮换");

        // 旧会话 ID 立即失效（卡片不存在）。
        let error = context.service.get_client(&old_session_id).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpClientNotFound);
        // 不存在的会话轮换报同样错误。
        let error = context.service.rotate_client_session_id(&old_session_id).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpClientNotFound);

        // 令牌归属已迁移：新会话下可解密且明文不变，认证仍通过并累计到新会话。
        let revealed = context.service.get_client_secret(&card.session_id).unwrap();
        assert_eq!(revealed.plain_token, secret.plain_token);
        context.service.authenticate(&secret.plain_token).unwrap().unwrap();
        let card2 = context.service.get_client(&card.session_id).unwrap();
        assert_eq!(card2.call_count, 2);

        // 数据库完整性：不存在仍指旧 id 的 Token。
        let connection = context.database.open().unwrap();
        let stale: i64 = connection
            .query_row(
                "SELECT count(*) FROM mcp_token WHERE session_id=$id;",
                params![old_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0);

        // 删除新会话一并清理。
        context.service.delete_client(&card.session_id).unwrap();
        let error = context.service.get_client(&card.session_id).unwrap_err();
        assert_eq!(error.code, ErrorCode::McpClientNotFound);
    }

    #[test]
    fn expired_token_cannot_authenticate_and_card_shows_expired() {
        let context = context();
        // 过期时间早于固定时钟（2026-08-15T08:00）。
        let mut request = client_request("过期工具");
        request.expires_at = Some("2026-08-14T00:00:00.0000000+00:00".to_string());
        let secret = context.service.create_client(&request).unwrap();
        assert_eq!(secret.client.status, McpClientStatus::Expired);
        assert!(context.service.authenticate(&secret.plain_token).unwrap().is_none());
    }

    #[test]
    fn normalize_client_key_matches_csharp_rules() {
        assert_eq!(normalize_client_key("  Claude 桌面 "), "claude桌面");
        assert_eq!(normalize_client_key("ABC"), "abc");
        assert_eq!(normalize_client_key("a b\tc"), "abc");
    }

    #[test]
    fn compute_status_derives_from_timestamps() {
        let now = "2026-08-15T08:00:00.0000000+00:00";
        assert_eq!(compute_status(None, None, now), McpClientStatus::Active);
        assert_eq!(
            compute_status(Some("2026-08-15T07:00:00.0000000+00:00"), None, now),
            McpClientStatus::Revoked
        );
        assert_eq!(
            compute_status(None, Some("2026-08-14T00:00:00.0000000+00:00"), now),
            McpClientStatus::Expired
        );
        assert_eq!(
            compute_status(None, Some("2026-08-16T00:00:00.0000000+00:00"), now),
            McpClientStatus::Active
        );
        // 吊销优先于过期。
        assert_eq!(
            compute_status(
                Some("2026-08-15T07:00:00.0000000+00:00"),
                Some("2026-08-14T00:00:00.0000000+00:00"),
                now
            ),
            McpClientStatus::Revoked
        );
    }
}
