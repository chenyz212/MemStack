//! 迁移锁并发测试：双线程并发 open_initialized 同一低版本副本，必须串行迁移且终态 v10。

use std::path::PathBuf;
use std::sync::Arc;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
fn concurrent_open_initialized_converges_to_version_10() {
    let sample = repo_root().join("testdata/db-samples/v3.db");
    if !sample.exists() {
        eprintln!("跳过：缺少 v3.db 样本");
        return;
    }
    let directory = std::env::temp_dir().join(format!(
        "memstack-migration-concurrent-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let database_path = Arc::new(directory.join("memory.db"));
    std::fs::copy(&sample, database_path.as_path()).unwrap();

    let mut handles = Vec::new();
    for _ in 0..2 {
        let path = Arc::clone(&database_path);
        handles.push(std::thread::spawn(move || memory_storage::open_initialized(&path)));
    }
    for handle in handles {
        let connection = handle.join().expect("线程不得 panic").expect("并发打开必须成功");
        assert_eq!(
            connection
                .query_row("PRAGMA user_version;", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            10
        );
    }
    let connection = memory_storage::open_read_only_connection(&database_path).unwrap();
    let integrity: String = connection
        .query_row("PRAGMA integrity_check;", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let _ = std::fs::remove_dir_all(&directory);
}
