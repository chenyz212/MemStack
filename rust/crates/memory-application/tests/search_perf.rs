//! seed-20k 检索性能回归（参数沿自历史 C# BenchRunner 基线，现为 Rust 稳定基线）。
//!
//! 复制 seed-20k.db（含 -wal/-shm）到临时目录，预热 5 次后对三组查询
//! （中文「发布验证」、英文 `rust`、混合「架构设计 发布验证」）各测 100 次，
//! P95 取升序第 ⌈n*0.95⌉ 位；结果落盘 `testdata/bench-rust-search-result.json`
//! 供验收报告对照。
//!
//! P95 ≥ 100ms 不硬阻塞：输出实测值并把偏差写入结果文件（计划 §三 T5 慢机容差）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use memory_application::db::Database;
use memory_application::search_service::{QueryVectorProvider, SearchService};
use memory_domain::SearchRequest;
use serde_json::json;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// keyword 模式：查询向量不可用（基线语义 `semantic_enabled=false`）。
struct NoVectors;

impl QueryVectorProvider for NoVectors {
    fn try_create(&self, _query: &str) -> Option<Vec<f32>> {
        None
    }
}

/// 基线请求：无过滤、limit=10、语义关闭。
fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        scope: None,
        project_id: None,
        memory_type: None,
        tag: None,
        limit: 10,
        semantic_enabled: false,
    }
}

#[test]
fn seed20k_search_p95_baseline() {
    let seed = repo_root().join("testdata/db-samples/seed-20k.db");
    if !seed.exists() {
        eprintln!("跳过：缺少 seed-20k.db 样本");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("bench.db");
    std::fs::copy(&seed, &database_path).unwrap();
    for suffix in ["-wal", "-shm"] {
        let sidecar = repo_root().join(format!("testdata/db-samples/seed-20k.db{suffix}"));
        if sidecar.exists() {
            std::fs::copy(&sidecar, database_path.with_file_name(format!("bench.db{suffix}"))).unwrap();
        }
    }
    drop(memory_storage::open_initialized(&database_path).unwrap());
    let service = SearchService::new(Database::new(database_path), Arc::new(NoVectors));

    // 预热数据库页缓存（5 次「发布验证」，与基线同参数）。
    for _ in 0..5 {
        let results = service.search(&request("发布验证")).unwrap();
        assert!(!results.is_empty(), "预热查询必须命中种子数据");
    }

    let groups = [
        ("中文关键词", "发布验证"),
        ("英文关键词", "rust"),
        ("混合关键词", "架构设计 发布验证"),
    ];
    let mut rows = Vec::new();
    let mut deviations = Vec::new();
    println!("二万条数据检索基线（Rust SearchService::search，100 次/组）：");
    for (name, query) in groups {
        let mut elapsed: Vec<u64> = Vec::with_capacity(100);
        let mut hit_batches = 0u32;
        for _ in 0..100 {
            let start = Instant::now();
            let results = service.search(&request(query)).unwrap();
            elapsed.push(start.elapsed().as_millis() as u64);
            if !results.is_empty() {
                hit_batches += 1;
            }
        }
        elapsed.sort_unstable();
        let p95 = elapsed[(elapsed.len() as f64 * 0.95).ceil() as usize - 1];
        let mean = elapsed.iter().sum::<u64>() as f64 / elapsed.len() as f64;
        println!("  {name}：P95={p95}ms，均值={mean:.1}ms，命中组数={hit_batches}/100");
        if p95 >= 100 {
            let note = format!("{name} P95={p95}ms 超出 100ms 预算（慢机容差不阻塞）");
            eprintln!("偏差：{note}");
            deviations.push(note);
        }
        rows.push(json!({
            "name": name,
            "p95Ms": p95,
            "meanMs": (mean * 10.0).round() / 10.0,
            "hitBatches": hit_batches,
        }));
    }

    let report = json!({
        "measuredAt": chrono::Utc::now().to_rfc3339(),
        "rows": rows,
        "deviations": deviations,
    });
    let report_path = repo_root().join("testdata/bench-rust-search-result.json");
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap() + "\n").unwrap();
    println!("结果已写入 {}", report_path.display());

    // 命中行为与基线一致：中文/混合 100 命中，英文走模糊回退 0 命中。
    assert_eq!(rows[0]["hitBatches"], 100, "中文关键词必须全部命中");
    assert_eq!(rows[2]["hitBatches"], 100, "混合关键词必须全部命中");
    assert_eq!(rows[1]["hitBatches"], 0, "英文 rust 应走模糊回退（种子无英文正文）");
}
