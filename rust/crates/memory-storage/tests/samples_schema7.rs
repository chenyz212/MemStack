//! 阶段 2 高风险验证：用 Rust 打开 C# 生成的 schema 7 样本库并校验内容与格式兼容性。
//!
//! 样本由 `MemStack.Tools samples` 生成；缺失时跳过（保证无样本环境可编译运行）。

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn sample(name: &str) -> Option<PathBuf> {
    let path = repo_root().join("testdata").join("db-samples").join(name);
    path.exists().then_some(path)
}

fn table_count(connection: &rusqlite::Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT count(*) FROM {table};"), [], |row| row.get(0))
        .unwrap()
}

#[test]
fn opens_schema7_sample_readonly() {
    let Some(path) = sample("v7.db") else {
        eprintln!("跳过：缺少 v7.db 样本");
        return;
    };
    let connection = memory_storage::open_read_only_connection(&path).unwrap();
    memory_storage::verify_fts5(&connection).unwrap();
    assert_eq!(memory_storage::detect_schema_version(&connection).unwrap(), 7);
}

#[test]
fn opens_full_sample_and_validates_content() {
    let Some(path) = sample("full-sample.db") else {
        eprintln!("跳过：缺少 full-sample.db 样本");
        return;
    };
    let connection = memory_storage::open_read_only_connection(&path).unwrap();
    memory_storage::verify_fts5(&connection).unwrap();
    assert_eq!(memory_storage::detect_schema_version(&connection).unwrap(), 7);

    // 表计数与样本设计一致：2 项目、5 记忆、2 候选、1 会话、1 Token、1 后台任务。
    assert_eq!(table_count(&connection, "project"), 2);
    assert_eq!(table_count(&connection, "memory"), 5);
    assert_eq!(table_count(&connection, "memory_candidate"), 2);
    assert_eq!(table_count(&connection, "mcp_client_session"), 1);
    assert_eq!(table_count(&connection, "mcp_token"), 1);
    assert_eq!(table_count(&connection, "background_task"), 1);

    // FTS5 中文查询：按 C# 分词风格（单字 + 二元）构造 MATCH 表达式。
    let hits: i64 = connection
        .query_row(
            "SELECT count(*) FROM memory_fts WHERE memory_fts MATCH '\"迁 移\" OR 迁移';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(hits > 0, "中文 FTS 查询必须命中样本数据");

    // 时间格式：可解析 C# DateTimeOffset.ToString("O") 的 7 位小数 UTC 文本。
    let updated_at: String = connection
        .query_row("SELECT updated_at FROM memory LIMIT 1;", [], |row| row.get(0))
        .unwrap();
    chrono::DateTime::parse_from_rfc3339(&updated_at)
        .or_else(|_| {
            // 兼容 +00:00 偏移下 7 位小数的完整往返格式。
            chrono::DateTime::parse_from_str(&updated_at, "%Y-%m-%dT%H:%M:%S%.f%:z")
        })
        .expect("必须能解析 C# 往返格式时间文本");
    // C# "O" 往返格式固定输出 7 位小数（样本可能全为 0），校验格式而非数值。
    if let Some(fraction) = updated_at.split('.').nth(1) {
        let digits = fraction.chars().take_while(|c| c.is_ascii_digit()).count();
        assert_eq!(digits, 7, "C# 往返格式固定 7 位小数：{updated_at}");
    } else {
        panic!("样本时间文本必须包含小数部分：{updated_at}");
    }

    // GUID：session id 必须是标准带连字符格式。
    let session_id: String = connection
        .query_row("SELECT id FROM mcp_client_session LIMIT 1;", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_id.len(), 36);
    assert_eq!(session_id.matches('-').count(), 4);

    // keywords_json：保持 JSON 数组格式。
    let keywords_json: String = connection
        .query_row("SELECT keywords_json FROM memory LIMIT 1;", [], |row| row.get(0))
        .unwrap();
    let keywords: Vec<String> = serde_json::from_str(&keywords_json).unwrap();
    assert!(!keywords.is_empty());
}

#[test]
fn decodes_embedding_vector_blob_little_endian() {
    let Some(path) = sample("full-sample.db") else {
        eprintln!("跳过：缺少 full-sample.db 样本");
        return;
    };
    let connection = memory_storage::open_read_only_connection(&path).unwrap();
    let (dimensions, blob): (i64, Vec<u8>) = connection
        .query_row(
            "SELECT dimensions, vector_blob FROM memory_embedding LIMIT 1;",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(dimensions, 4);
    assert_eq!(blob.len(), (dimensions * 4) as usize);
    let mut floats = Vec::with_capacity(dimensions as usize);
    for index in 0..dimensions as usize {
        let bytes: [u8; 4] = blob[index * 4..index * 4 + 4].try_into().unwrap();
        floats.push(f32::from_le_bytes(bytes));
    }
    assert_eq!(floats, [0.1, 0.2, 0.3, 0.4]);
}

#[test]
fn legacy_java_database_triggers_desktop_fallback() {
    let Some(legacy) = sample("legacy-java.db") else {
        eprintln!("跳过：缺少 legacy-java.db 样本");
        return;
    };
    let directory = legacy.parent().unwrap();
    assert!(!memory_storage::is_desktop_schema(&legacy).unwrap());

    // 场景一：数据目录只有旧 Java 库 → 解析到 desktop-memory.db。
    let only_legacy = directory.join("fallback-only-legacy");
    let _ = std::fs::remove_dir_all(&only_legacy);
    std::fs::create_dir_all(&only_legacy).unwrap();
    std::fs::copy(&legacy, only_legacy.join("memory.db")).unwrap();
    let resolved = memory_storage::resolve_desktop_database_path(&only_legacy).unwrap();
    assert!(resolved.ends_with("desktop-memory.db"));

    // 场景二：空目录 → 解析到 memory.db。
    let empty = directory.join("fallback-empty");
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let resolved = memory_storage::resolve_desktop_database_path(&empty).unwrap();
    assert!(resolved.ends_with("memory.db"));

    let _ = std::fs::remove_dir_all(&only_legacy);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn opens_all_schema_chain_snapshots() {
    for version in 1..=7 {
        let Some(path) = sample(&format!("v{version}.db")) else {
            eprintln!("跳过：缺少 v{version}.db 样本");
            return;
        };
        let connection = memory_storage::open_read_only_connection(&path).unwrap();
        assert_eq!(
            memory_storage::detect_schema_version(&connection).unwrap(),
            version,
            "v{version}.db 快照结构版本必须等于 {version}"
        );
    }
}
