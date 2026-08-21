//! 双真实进程迁移竞争测试：两个 MemStack-MCP.exe 并发指向同一 v3 副本，
//! 迁移发生在鉴权之前（子进程因无效 Token 非零退出属预期），
//! 终态必须是 schema 8 且完整性 ok（迁移执行计划 §6.3）。

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
fn two_mcp_processes_race_migration_and_converge() {
    let sample = repo_root().join("testdata/db-samples/v3.db");
    if !sample.exists() {
        eprintln!("跳过：缺少 v3.db 样本");
        return;
    }
    let directory = std::env::temp_dir().join(format!("memstack-mcp-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let database_path = directory.join("memory.db");
    std::fs::copy(&sample, &database_path).unwrap();

    let mut children = Vec::new();
    for _ in 0..2 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_MemStack-MCP"))
            .env("MEMSTACK_DB_PATH", &database_path)
            .env("MEMSTACK_TOKEN", "uam_race_invalid_token")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 MemStack-MCP 失败");
        // 立即关闭 stdin 触发 EOF；鉴权失败先于读循环发生，退出码非 0 属预期。
        drop(child.stdin.take());
        children.push(child);
    }
    for child in children {
        let output = child.wait_with_output().expect("等待子进程失败");
        // 迁移在鉴权之前完成，因此即便鉴权失败也要求进程走到了退出阶段。
        assert!(output.status.code().is_some(), "进程不得被信号终止");
    }

    let connection = memory_storage::open_read_only_connection(&database_path).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version;", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 8, "双进程竞争后必须收敛到 schema 8");
    let integrity: String = connection
        .query_row("PRAGMA integrity_check;", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let _ = std::fs::remove_dir_all(&directory);
}

/// 独立进程直接退出场景：空 stdin 干净退出（回归保护）。
#[test]
fn stdin_eof_exits_cleanly() {
    let sample = repo_root().join("testdata/db-samples/full-sample.db");
    let token_file = repo_root().join("testdata/golden/sample-token.json");
    if !sample.exists() || !token_file.exists() {
        eprintln!("跳过：缺少样本");
        return;
    }
    let directory = std::env::temp_dir().join(format!("memstack-mcp-eof-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let database_path = directory.join("memory.db");
    std::fs::copy(&sample, &database_path).unwrap();
    let token: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&token_file).unwrap()).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_MemStack-MCP"))
        .env("MEMSTACK_DB_PATH", &database_path)
        .env("MEMSTACK_TOKEN", token["plainToken"].as_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"\n").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let _ = std::fs::remove_dir_all(&directory);
}
