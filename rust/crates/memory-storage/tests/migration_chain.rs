//! v1→v7 迁移链验收测试：全链升级、与 C# 样本结构等价、v7 语义修复与段级原子性。
//!
//! 样本缺失时跳过（保持无样本环境可独立运行）。

use std::path::{Path, PathBuf};

use rusqlite::Connection;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn sample(name: &str) -> Option<PathBuf> {
    let path = repo_root().join("testdata").join("db-samples").join(name);
    path.exists().then_some(path)
}

fn work_copy(label: &str, source: &Path) -> PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "memstack-migration-chain-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let target = directory.join("memory.db");
    std::fs::copy(source, &target).unwrap();
    target
}

fn user_version(connection: &Connection) -> i64 {
    connection
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .unwrap()
}

fn integrity(connection: &Connection) -> String {
    connection
        .query_row("PRAGMA integrity_check;", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn open_initialized_on_empty_file_creates_full_schema() {
    let directory = std::env::temp_dir().join(format!(
        "memstack-migration-empty-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("memory.db");
    std::fs::write(&path, b"").unwrap();

    let connection = memory_storage::open_initialized(&path).unwrap();
    assert_eq!(user_version(&connection), 8);
    assert_eq!(integrity(&connection), "ok");
    let tables: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type IN ('table','view') AND name IN
                ('app_setting','project','memory','memory_revision','memory_embedding','memory_edge',
                 'memory_candidate','mcp_token','mcp_client_session','background_task','memory_fts');",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 11);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn open_initialized_upgrades_each_sample_to_version_8() {
    for version in 1..=7 {
        let Some(source) = sample(&format!("v{version}.db")) else {
            eprintln!("跳过：缺少 v{version}.db 样本");
            return;
        };
        let path = work_copy(&format!("chain-v{version}"), &source);
        let connection = memory_storage::open_initialized(&path).unwrap();
        assert_eq!(user_version(&connection), 8, "v{version} 副本必须升级到 8");
        assert_eq!(integrity(&connection), "ok");
        drop(connection);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

/// Rust 迁移 v1 副本后的库结构必须与 C# 迁移链产物（v7.db 样本）一致；
/// v8 在 mcp_token 上追加了 3 个活动列（Rust 独有），按列级对照排除后必须完全一致。
#[test]
fn rust_migration_matches_csharp_v7_schema() {
    let Some(v1) = sample("v1.db") else {
        eprintln!("跳过：缺少 v1.db 样本");
        return;
    };
    let Some(csharp_v7) = sample("v7.db") else {
        eprintln!("跳过：缺少 v7.db 样本");
        return;
    };
    let rust_path = work_copy("schema-equality", &v1);
    let rust_connection = memory_storage::open_initialized(&rust_path).unwrap();
    let csharp_connection = memory_storage::open_read_only_connection(&csharp_v7).unwrap();

    let schema_rows = |connection: &Connection| {
        connection
            .prepare(
                "SELECT type, name, coalesce(sql,'') FROM sqlite_master \
                 WHERE name NOT LIKE 'sqlite_%' AND NOT (type='table' AND name='mcp_token') \
                 ORDER BY type, name;",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    let rust_schema = schema_rows(&rust_connection);
    let csharp_schema = schema_rows(&csharp_connection);
    assert_eq!(
        rust_schema,
        csharp_schema,
        "除 v8 新增列所在的 mcp_token 外，Rust 迁移产物与 C# v7 样本结构必须逐行一致（共 {} 行）",
        csharp_schema.len()
    );

    // mcp_token：v7 列集合 + 按序追加的 3 个 v8 活动列。
    let columns = |connection: &Connection| {
        connection
            .prepare("SELECT name FROM pragma_table_info('mcp_token');")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let mut expected = columns(&csharp_connection);
    expected.extend([
        "last_memory_action".to_string(),
        "last_action_scope".to_string(),
        "last_action_at".to_string(),
    ]);
    assert_eq!(columns(&rust_connection), expected, "v8 仅按序追加 3 个活动列");
    let _ = std::fs::remove_dir_all(rust_path.parent().unwrap());
}

/// v7 语义冒烟：v6 副本中 32 字符无连字符 session id 迁移后必须变为标准 GUID 格式。
#[test]
fn v7_migration_fixes_unguid_session_ids() {
    let Some(v6) = sample("v6.db") else {
        eprintln!("跳过：缺少 v6.db 样本");
        return;
    };
    let path = work_copy("v7-guid-fix", &v6);
    {
        let connection = memory_storage::open_connection(&path).unwrap();
        connection
            .execute_batch(
                "INSERT INTO mcp_client_session(id, client_key, display_name, client_version, transport, \
                 last_seen_at, call_count, created_at, updated_at) VALUES \
                 ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','probe-key','探测',NULL,'http',NULL,0,\
                 '2026-01-01T00:00:00.0000000+00:00','2026-01-01T00:00:00.0000000+00:00');
                 INSERT INTO mcp_token(id,name,token_hash,access_mode,project_scope_json,created_at,session_id) \
                 VALUES('probe-token','探测','probe-hash','Read','[]',\
                 '2026-01-01T00:00:00.0000000+00:00','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa');",
            )
            .unwrap();
    }

    let connection = memory_storage::open_initialized(&path).unwrap();
    assert_eq!(user_version(&connection), 8);
    let session_id: String = connection
        .query_row(
            "SELECT id FROM mcp_client_session WHERE client_key='probe-key';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(session_id, "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
    let token_session: String = connection
        .query_row("SELECT session_id FROM mcp_token WHERE id='probe-token';", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(token_session, session_id);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// 段级原子性：v6 段失败（同名双有效 Token 违反唯一索引）必须整体回滚，
/// 版本停留 v5、新表不落库；修复数据后重试可继续升级。
#[test]
fn failed_migration_step_rolls_back_atomically_and_recovers() {
    let Some(v5) = sample("v5.db") else {
        eprintln!("跳过：缺少 v5.db 样本");
        return;
    };
    let path = work_copy("v6-atomic", &v5);
    {
        let connection = memory_storage::open_connection(&path).unwrap();
        connection
            .execute_batch(
                "INSERT INTO mcp_token(id,name,token_hash,access_mode,project_scope_json,created_at,display_name) VALUES
                 ('t1','重复','hash-1','Read','[]','2026-01-01T00:00:00.0000000+00:00','重复客户端'),
                 ('t2','重复','hash-2','Read','[]','2026-01-01T00:00:00.0000000+00:00','重复客户端');",
            )
            .unwrap();
    }

    // v6 段失败：两个同名有效 Token 会映射到同一会话，违反 uk_mcp_token_session_active。
    let failure = memory_storage::open_initialized(&path).unwrap_err();
    assert!(
        failure.message.contains("v6"),
        "错误信息应指向 v6 段：{}",
        failure.message
    );
    {
        let connection = memory_storage::open_connection(&path).unwrap();
        assert_eq!(user_version(&connection), 5, "失败段版本号不得落库");
        let session_tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='mcp_client_session';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session_tables, 0, "失败段的 DDL 必须回滚");
        // 修复数据：吊销其中一个 Token 后重试。
        connection
            .execute(
                "UPDATE mcp_token SET revoked_at='2026-01-02T00:00:00.0000000+00:00' WHERE id='t2';",
                [],
            )
            .unwrap();
    }

    let connection = memory_storage::open_initialized(&path).unwrap();
    assert_eq!(user_version(&connection), 8, "修复后必须能继续升级到 8");
    assert_eq!(integrity(&connection), "ok");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
