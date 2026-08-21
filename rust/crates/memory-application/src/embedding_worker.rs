//! 通用后台任务消费者（Embedding + 记忆图谱关系重算）。
//!
//! - 桌面进程是唯一消费者（架构决策 11）：本模块提供 `spawn_embedding_worker`
//!   启动独立线程；MCP 进程只入队任务，不启动 Worker。
//! - 任务类型：`EMBED_MEMORY`、`REBUILD_EMBEDDING`、`REBUILD_GRAPH_MEMORY`、
//!   `REBUILD_GRAPH_ALL`（图谱任务在 Embedding 成功后再次重算该记忆）。
//! - 领取：事务内取最早到期 PENDING，CAS 更新为 RUNNING。
//! - 退避：失败后 `attempt_count+1`、`next_attempt_at = now + 2^attempts` 秒，
//!   3 次后置 FAILED；FAILED 仅保留最近 20 条。
//! - 空闲 2 秒轮询（分片睡眠保证 stop 及时响应）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use memory_domain::BusinessError;
use rusqlite::params;

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::embedding_service::EmbeddingService;
use crate::graph_service::GraphService;
use crate::sqlite_errors::map_sqlite_error;

/// 空闲轮询间隔（与 C# `Task.Delay(2s)` 一致）。
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 睡眠分片：保证 stop 信号在 100ms 内被感知。
const SLEEP_SLICE: Duration = Duration::from_millis(100);
/// 最大尝试次数（与 C# `attempts >= 3` 一致）。
const MAX_ATTEMPTS: i64 = 3;
/// FAILED 任务保留数量（与 C# `LIMIT 20` 一致）。
const FAILED_RETENTION: i64 = 20;

/// 已领取的后台任务（对应 C# `ClaimedTask`）。
#[derive(Debug, Clone)]
pub struct ClaimedTask {
    pub id: String,
    pub task_type: String,
    pub target_id: Option<String>,
    pub attempt_count: i64,
}

/// 通用后台任务执行器（Embedding + 图谱关系重算）。
pub struct EmbeddingWorker {
    database: Database,
    embedding: Arc<EmbeddingService>,
    graph: Arc<GraphService>,
    clock: Arc<dyn Clock>,
    stop: Arc<AtomicBool>,
}

impl EmbeddingWorker {
    /// 创建执行器（一般经 `spawn_embedding_worker` 使用）。
    pub fn new(
        database: Database,
        embedding: Arc<EmbeddingService>,
        graph: Arc<GraphService>,
        clock: Arc<dyn Clock>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        Self {
            database,
            embedding,
            graph,
            clock,
            stop,
        }
    }

    /// 是否收到停止信号。
    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// 执行一轮「领取 → 处理」；返回是否领到了任务（供线程循环与测试复用）。
    pub fn run_one_cycle(&self) -> bool {
        match self.claim() {
            Ok(Some(task)) => {
                self.execute(&task);
                true
            }
            Ok(None) => false,
            Err(error) => {
                eprintln!("[embedding-worker] 领取任务失败：{error}");
                false
            }
        }
    }

    /// 线程主循环：有任务连续处理，空闲时间片轮询，stop 后退出。
    pub fn run(&self) {
        while !self.is_stopping() {
            if self.run_one_cycle() {
                continue;
            }
            let mut remaining = IDLE_POLL_INTERVAL;
            while remaining > Duration::ZERO && !self.is_stopping() {
                let slice = remaining.min(SLEEP_SLICE);
                std::thread::sleep(slice);
                remaining -= slice;
            }
        }
    }

    /// 原子领取最早到期任务（SQL 与 C# `ClaimAsync` 一致）。
    fn claim(&self) -> Result<Option<ClaimedTask>, BusinessError> {
        let mut connection = self.database.open()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let now_text = format_storage_time(self.clock.now_utc());
        let row: Option<(String, String, Option<String>, i64)> = transaction
            .query_row(
                "SELECT id,task_type,target_id,attempt_count FROM background_task \
                 WHERE status='PENDING' AND (next_attempt_at IS NULL OR next_attempt_at <= $now) \
                 ORDER BY created_at LIMIT 1;",
                params![now_text],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        let Some((id, task_type, target_id, attempt_count)) = row else {
            transaction.commit().map_err(map_sqlite_error)?;
            return Ok(None);
        };
        let updated = transaction
            .execute(
                "UPDATE background_task SET status='RUNNING',updated_at=$updated WHERE id=$id AND status='PENDING';",
                params![now_text, id],
            )
            .map_err(map_sqlite_error)?;
        if updated == 0 {
            // CAS 失败：别的消费者已领取（C# 回滚返回 null）。
            return Ok(None);
        }
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(Some(ClaimedTask {
            id,
            task_type,
            target_id,
            attempt_count,
        }))
    }

    /// 执行一条任务：成功删除，失败记录有限重试（异常不外抛，与 C# `ExecuteTaskAsync` 一致）。
    fn execute(&self, task: &ClaimedTask) {
        let result = (|| -> Result<(), BusinessError> {
            match task.task_type.as_str() {
                "REBUILD_EMBEDDING" => self.queue_all_memories()?,
                "EMBED_MEMORY" => {
                    if let Some(memory_id) = task.target_id.as_deref() {
                        self.embedding.process_memory(memory_id)?;
                        // Embedding 成功后再次重算该记忆的关系（计划 §后台关系计算）。
                        self.graph.recompute_memory(memory_id)?;
                    }
                }
                "REBUILD_GRAPH_MEMORY" => {
                    if let Some(memory_id) = task.target_id.as_deref() {
                        self.graph.recompute_memory(memory_id)?;
                    }
                }
                "REBUILD_GRAPH_ALL" => self.graph.recompute_all()?,
                _ => {}
            }
            self.delete_task(&task.id)?;
            Ok(())
        })();
        if let Err(error) = result
            && let Err(record_error) = self.fail_task(task, &error)
        {
            eprintln!("[embedding-worker] 记录任务失败：{record_error}（原错误：{error}）");
        }
    }

    /// 为全部可云端处理的活动记忆创建去重任务（SQL 与 C# `QueueAllMemoriesAsync` 一致）。
    fn queue_all_memories(&self) -> Result<(), BusinessError> {
        let connection = self.database.open()?;
        let now_text = format_storage_time(self.clock.now_utc());
        connection
            .execute(
                "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
                 SELECT lower(hex(randomblob(16))),'EMBED_MEMORY',m.id,'PENDING',0,$now,NULL,NULL,$now,$now \
                 FROM memory m WHERE m.status='Active' AND m.cloud_processing_allowed=1 \
                 AND NOT EXISTS(SELECT 1 FROM background_task t WHERE t.task_type='EMBED_MEMORY' AND t.target_id=m.id AND t.status IN ('PENDING','RUNNING'));",
                params![now_text],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 删除成功任务。
    fn delete_task(&self, id: &str) -> Result<(), BusinessError> {
        let connection = self.database.open()?;
        connection
            .execute("DELETE FROM background_task WHERE id=$id;", params![id])
            .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 更新失败任务并清理超量 FAILED 记录（与 C# `FailTaskAsync` 一致）。
    fn fail_task(&self, task: &ClaimedTask, error: &BusinessError) -> Result<(), BusinessError> {
        let attempts = task.attempt_count + 1;
        let status = if attempts >= MAX_ATTEMPTS { "FAILED" } else { "PENDING" };
        let now = self.clock.now_utc();
        // 2^attempts 秒退避：1→2s、2→4s、3→8s。
        let next = now + chrono::Duration::seconds(1_i64 << attempts);
        let message: String = error.message.chars().take(500).collect();
        let connection = self.database.open()?;
        connection
            .execute(
                "UPDATE background_task SET status=$status,attempt_count=$attempts,next_attempt_at=$next,error_code=$code,error_message=$message,updated_at=$updated WHERE id=$id;",
                params![
                    status,
                    attempts,
                    format_storage_time(next),
                    error.code.as_str(),
                    message,
                    format_storage_time(now),
                    task.id
                ],
            )
            .map_err(map_sqlite_error)?;
        connection
            .execute(
                &format!(
                    "DELETE FROM background_task WHERE status='FAILED' AND id NOT IN \
                     (SELECT id FROM background_task WHERE status='FAILED' ORDER BY updated_at DESC LIMIT {FAILED_RETENTION});"
                ),
                [],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }
}

/// Worker 线程句柄：`stop()` 置位停止信号并等待线程退出。
pub struct WorkerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WorkerHandle {
    /// 置位停止信号并等待线程退出（幂等）。
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// 停止信号是否已置位。
    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// 启动通用后台任务线程（桌面进程唯一消费者）。
pub fn spawn_embedding_worker(
    database: Database,
    embedding: Arc<EmbeddingService>,
    graph: Arc<GraphService>,
    clock: Arc<dyn Clock>,
) -> WorkerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let worker = Arc::new(EmbeddingWorker::new(database, embedding, graph, clock, stop.clone()));
    let thread = std::thread::Builder::new()
        .name("embedding-worker".to_string())
        .spawn(move || worker.run())
        .expect("启动 embedding-worker 线程失败");
    WorkerHandle {
        stop,
        thread: Some(thread),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::embedding_service::StubEmbeddingHttp;
    use crate::ids::FixedIdGenerator;
    use crate::memory_service::MemoryService;
    use chrono::TimeZone;
    use memory_domain::{MemoryScope, SaveMemoryRequest};

    struct TestContext {
        #[allow(dead_code)]
        directory: tempfile::TempDir,
        database: Database,
        clock: Arc<FixedClock>,
        ids: Arc<FixedIdGenerator>,
        embedding: Arc<EmbeddingService>,
        graph: Arc<GraphService>,
    }

    fn context(fail_http: bool) -> TestContext {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("worker.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let clock = Arc::new(FixedClock::new(
            chrono::Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap(),
        ));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=128)
                .map(|index| format!("{index:08}-0000-4000-8000-{index:012}"))
                .collect(),
        ));
        let embedding = Arc::new(EmbeddingService::new(
            Database::new(database_path.clone()),
            clock.clone(),
            ids.clone(),
            Arc::new(StubEmbeddingHttp {
                dimensions: 128,
                fail: fail_http,
            }),
        ));
        let graph = Arc::new(GraphService::new(
            Database::new(database_path.clone()),
            clock.clone(),
            ids.clone(),
        ));
        TestContext {
            directory,
            database: Database::new(database_path),
            clock,
            ids,
            embedding,
            graph,
        }
    }

    fn worker(context: &TestContext) -> EmbeddingWorker {
        EmbeddingWorker::new(
            context.database.clone(),
            context.embedding.clone(),
            context.graph.clone(),
            context.clock.clone(),
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn create_memory(context: &TestContext, title: &str, cloud_allowed: bool) -> String {
        let memories = MemoryService::new(context.database.clone(), context.clock.clone(), context.ids.clone());
        memories
            .create(&SaveMemoryRequest {
                scope: MemoryScope::Personal,
                project_id: None,
                title: title.to_string(),
                summary: "摘要".to_string(),
                content: format!("{title} 正文内容"),
                memory_type: "NOTE".to_string(),
                keywords: vec![],
                tags: vec![],
                importance: 3,
                is_favorite: false,
                is_pinned: false,
                cloud_processing_allowed: cloud_allowed,
                expected_version: None,
            })
            .unwrap()
            .id
    }

    /// 直接插入一条任务（绕过服务层，精确控制 next_attempt_at）。
    fn insert_task(context: &TestContext, id: &str, task_type: &str, target: Option<&str>, next: Option<&str>) {
        let now = format_storage_time(context.clock.now_utc());
        context
            .database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
                 VALUES($id,$type,$target,'PENDING',0,$next,NULL,NULL,$now,$now);",
                params![id, task_type, target, next, now],
            )
            .unwrap();
    }

    /// 直接写入已启用的 Embedding 配置（不触发 save 的重建入队，供失败路径测试）。
    fn enable_settings_raw(context: &TestContext) {
        let encrypted = memory_platform::dpapi::protect("sk-test-key-123456").unwrap();
        let now = format_storage_time(context.clock.now_utc());
        let connection = context.database.open().unwrap();
        for (key, value) in [
            ("embedding.base_url", "https://api.example.com/v1".to_string()),
            ("embedding.model", "text-embedding-3-small".to_string()),
            ("embedding.api_key", encrypted),
            ("embedding.dimensions", "128".to_string()),
            ("embedding.enabled", "True".to_string()),
        ] {
            connection
                .execute(
                    "INSERT INTO app_setting(setting_key,setting_value,updated_at) VALUES($key,$value,$updated) \
                     ON CONFLICT(setting_key) DO UPDATE SET setting_value=excluded.setting_value,updated_at=excluded.updated_at;",
                    params![key, value, now],
                )
                .unwrap();
        }
    }

    /// 清空全部任务：create() 会对 cloud_allowed 记忆自动入队 EMBED，测试需清场后精确控制。
    fn clear_tasks(context: &TestContext) {
        context
            .database
            .open()
            .unwrap()
            .execute("DELETE FROM background_task;", [])
            .unwrap();
    }

    fn task_status(context: &TestContext, id: &str) -> (String, i64, Option<String>) {
        context
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT status,attempt_count,next_attempt_at FROM background_task WHERE id=$id;",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn count_tasks(context: &TestContext, condition: &str) -> i64 {
        context
            .database
            .open()
            .unwrap()
            .query_row(
                &format!("SELECT count(*) FROM background_task WHERE {condition};"),
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn claim_is_exclusive_between_workers() {
        let context = context(false);
        insert_task(&context, "t1", "EMBED_MEMORY", Some("any"), None);
        let first = worker(&context);
        let second = worker(&context);
        let claimed_first = first.claim().unwrap();
        let claimed_second = second.claim().unwrap();
        assert!(claimed_first.is_some());
        // 第二个消费者领取时，任务已 RUNNING → 空。
        assert!(claimed_second.is_none());
        assert_eq!(count_tasks(&context, "status='RUNNING'"), 1);
    }

    #[test]
    fn rebuild_fans_out_deduped_embed_tasks() {
        let context = context(false);
        let active1 = create_memory(&context, "记忆一", true);
        let _active2 = create_memory(&context, "记忆二", true);
        let archived = {
            let id = create_memory(&context, "记忆三", true);
            let memories = MemoryService::new(context.database.clone(), context.clock.clone(), context.ids.clone());
            memories.archive(&id).unwrap();
            id
        };
        let _no_cloud = create_memory(&context, "记忆四", false);
        clear_tasks(&context);
        enable_settings_raw(&context);
        // REBUILD 先入队（最早），再预置 active1 的 PENDING EMBED（验证扇出去重）。
        insert_task(&context, "rebuild", "REBUILD_EMBEDDING", None, None);
        insert_task(&context, "pre-embed", "EMBED_MEMORY", Some(&active1), None);
        let worker = worker(&context);
        // 第一轮领取 REBUILD，扇出并删除自身：active1 去重、active2 新建。
        assert!(worker.run_one_cycle());
        assert_eq!(count_tasks(&context, "task_type='REBUILD_EMBEDDING'"), 0);
        assert_eq!(
            count_tasks(&context, "task_type='EMBED_MEMORY' AND status='PENDING'"),
            2
        );
        // 领取并处理两条 EMBED 任务：HTTP 成功 → 向量落库、任务删除。
        assert!(worker.run_one_cycle());
        assert!(worker.run_one_cycle());
        assert!(!worker.run_one_cycle());
        // 归档记忆永不产生向量。
        let connection = context.database.open().unwrap();
        let embedded: i64 = connection
            .query_row("SELECT count(*) FROM memory_embedding;", [], |row| row.get(0))
            .unwrap();
        let archived_vector: i64 = connection
            .query_row(
                "SELECT count(*) FROM memory_embedding WHERE memory_id=$id;",
                params![archived],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        assert_eq!(embedded, 2);
        assert_eq!(archived_vector, 0);
    }

    #[test]
    fn embed_failure_backs_off_and_fails_after_three_attempts() {
        let context = context(true);
        let memory_id = create_memory(&context, "会失败的记忆", true);
        clear_tasks(&context);
        // 配置启用（直接写库，避免 save() 触发 HTTP 测试与重建入队）。
        enable_settings_raw(&context);
        insert_task(&context, "embed-1", "EMBED_MEMORY", Some(&memory_id), None);
        let worker = worker(&context);
        let base = context.clock.now_utc();

        // 第 1 次失败 → PENDING，next = +2s。
        assert!(worker.run_one_cycle());
        let (status, attempts, next) = task_status(&context, "embed-1");
        assert_eq!((status.as_str(), attempts), ("PENDING", 1));
        assert_eq!(next, Some(format_storage_time(base + chrono::Duration::seconds(2))));

        // 未到退避时间 → 领取不到。
        context.clock.advance(chrono::Duration::seconds(1));
        assert!(!worker.run_one_cycle());

        // 第 2 次失败 → next = (T+2)+4s = T+6s。
        context.clock.advance(chrono::Duration::seconds(1));
        assert!(worker.run_one_cycle());
        let (status, attempts, next) = task_status(&context, "embed-1");
        assert_eq!((status.as_str(), attempts), ("PENDING", 2));
        assert_eq!(next, Some(format_storage_time(base + chrono::Duration::seconds(6))));

        // 第 3 次失败 → FAILED。
        context.clock.advance(chrono::Duration::seconds(4));
        assert!(worker.run_one_cycle());
        let (status, attempts, _) = task_status(&context, "embed-1");
        assert_eq!((status.as_str(), attempts), ("FAILED", 3));
        // 错误码来自 BusinessError。
        let error_code: String = context
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT error_code FROM background_task WHERE id='embed-1';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(error_code, "INTERNAL_ERROR");
    }

    #[test]
    fn failed_tasks_are_trimmed_to_twenty() {
        let context = context(true);
        let now = format_storage_time(context.clock.now_utc());
        let connection = context.database.open().unwrap();
        for index in 0..25 {
            connection
                .execute(
                    "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
                     VALUES($id,'X',NULL,'FAILED',3,NULL,NULL,NULL,$now,$now);",
                    params![format!("old-{index}"), now],
                )
                .unwrap();
        }
        drop(connection);
        let memory_id = create_memory(&context, "失败记忆", true);
        enable_settings_raw(&context);
        insert_task(&context, "embed-x", "EMBED_MEMORY", Some(&memory_id), None);
        let worker = worker(&context);
        // 连续失败 3 次触发裁剪。
        for _ in 0..3 {
            context.clock.advance(chrono::Duration::seconds(16));
            while worker.run_one_cycle() {}
        }
        assert_eq!(count_tasks(&context, "status='FAILED'"), FAILED_RETENTION);
    }

    #[test]
    fn unknown_task_type_deletes_silently() {
        let context = context(false);
        insert_task(&context, "weird", "UNKNOWN_TYPE", None, None);
        let worker = worker(&context);
        assert!(worker.run_one_cycle());
        assert_eq!(count_tasks(&context, "id='weird'"), 0);
    }

    #[test]
    fn spawn_stops_and_joins_cleanly() {
        let context = context(false);
        let handle = spawn_embedding_worker(
            context.database.clone(),
            context.embedding.clone(),
            context.graph.clone(),
            context.clock.clone(),
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(!handle.is_stopping());
        handle.stop();
        // stop 后可再次确认（幂等语义由消费方保证只调一次）。
    }

    #[test]
    fn graph_tasks_recompute_memory_and_all() {
        let context = context(false);
        let first = create_memory(&context, "图谱甲", false);
        let second = create_memory(&context, "图谱乙", false);
        clear_tasks(&context);
        // 共享关键词的任务参数：直接改库补 keywords（create 测试助手不支持关键词）。
        context
            .database
            .open()
            .unwrap()
            .execute(
                "UPDATE memory SET keywords_json='[\"共享\"]' WHERE id IN ($a,$b);",
                params![first, second],
            )
            .unwrap();
        insert_task(&context, "graph-mem", "REBUILD_GRAPH_MEMORY", Some(&first), None);
        insert_task(&context, "graph-all", "REBUILD_GRAPH_ALL", None, None);
        let worker = worker(&context);
        // 单记忆重算 → 该记忆与乙形成关键词边。
        assert!(worker.run_one_cycle());
        let edges: i64 = context
            .database
            .open()
            .unwrap()
            .query_row("SELECT count(*) FROM memory_edge;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edges, 1);
        // 全量重建正常完成并删除任务。
        assert!(worker.run_one_cycle());
        assert_eq!(count_tasks(&context, "task_type='REBUILD_GRAPH_ALL'"), 0);
        let edges: i64 = context
            .database
            .open()
            .unwrap()
            .query_row("SELECT count(*) FROM memory_edge;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edges, 1);
    }

    #[test]
    fn create_memory_queues_graph_recompute_task() {
        let context = context(false);
        create_memory(&context, "排队验证", false);
        // create() 会在同事务排队 REBUILD_GRAPH_MEMORY（不依赖云端处理开关）。
        assert_eq!(
            count_tasks(&context, "task_type='REBUILD_GRAPH_MEMORY' AND status='PENDING'"),
            1
        );
    }
}
