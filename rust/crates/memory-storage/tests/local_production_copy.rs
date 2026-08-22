//! 阶段 2 高风险验证：用 Rust 打开现有生产数据库副本（只读校验 + 写入冒烟）。
//!
//! 副本位于 `testdata/tmp-local-copy/memory.db`；由执行脚本从
//! `%LOCALAPPDATA%\MemStack\data\memory.db` 复制而来，验证后由脚本清理。
//! 副本不存在时跳过。

use std::path::PathBuf;

fn copy_path() -> Option<PathBuf> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../testdata/tmp-local-copy");
    // 与生产规则一致：memory.db 优先；识别为旧 Java 结构时回退 desktop-memory.db。
    for name in ["memory.db", "desktop-memory.db"] {
        let path = directory.join(name);
        if path.exists() && memory_storage::is_desktop_schema(&path).unwrap_or(false) {
            return Some(path);
        }
    }
    None
}

#[test]
fn opens_local_production_copy() {
    let Some(path) = copy_path() else {
        eprintln!("跳过：缺少生产库副本（先复制 memory.db 到 testdata/tmp-local-copy/）");
        return;
    };
    let connection = memory_storage::open_read_only_connection(&path).unwrap();
    memory_storage::verify_fts5(&connection).unwrap();
    let version = memory_storage::detect_schema_version(&connection).unwrap();
    assert!(
        version == 7 || version == 8 || version == 9 || version == 10,
        "生产库结构版本必须为 7/8/9/10（7=旧基线，8/9=历史 Rust 版本，10=当前版）：实际 {version}"
    );

    for table in [
        "project",
        "memory",
        "memory_candidate",
        "memory_revision",
        "mcp_token",
        "mcp_client_session",
        "memory_fts",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table};"), [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("读取 {table} 失败：{error}"));
        println!("{table}: {count} 行");
    }
}

#[test]
fn write_smoke_on_copy_never_touches_production() {
    let Some(path) = copy_path() else {
        eprintln!("跳过：缺少生产库副本");
        return;
    };
    let connection = memory_storage::open_connection(&path).unwrap();
    memory_storage::init_pragmas(&connection).unwrap();
    let updated = connection
        .execute(
            "UPDATE memory SET updated_at = updated_at WHERE id = (SELECT id FROM memory LIMIT 1);",
            [],
        )
        .unwrap();
    assert!(updated <= 1);
    connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE);", [], |_| Ok(()))
        .unwrap();
}
