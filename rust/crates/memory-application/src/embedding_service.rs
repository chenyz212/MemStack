//! Embedding 配置、向量请求与重建入队（C# `EmbeddingService` 等价实现）。
//!
//! - HTTP 走 `EmbeddingHttp` 抽象：生产用 ureq（30s 超时、Bearer、OpenAI 兼容
//!   `/embeddings`），测试注入固定向量，不依赖真实网络。
//! - API Key 经 DPAPI 加解密（与 C# `SecretProtector` 双向兼容）。
//! - 查询向量缓存 TTL 10 分钟，键为 SHA256 大写 hex（与 C# `CreateCacheKey` 一致）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use memory_domain::{
    BusinessError, EmbeddingSettingsView, EmbeddingStatus, EmbeddingTestResult, ErrorCode, RebuildTicket,
    SaveEmbeddingSettingsRequest,
};
use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::sqlite_errors::map_sqlite_error;

/// 查询向量缓存生命周期（与 C# `QueryCacheLifetime` 的 10 分钟一致）。
const QUERY_CACHE_LIFETIME_MINUTES: i64 = 10;

/// Embedding HTTP 端点抽象（生产 ureq / 测试注入）。
pub trait EmbeddingHttp: Send + Sync {
    /// 请求一次向量；失败返回错误消息（调用方决定吞掉还是让任务重试）。
    fn request_embedding(
        &self,
        base_url: &str,
        api_key: &str,
        model: &str,
        dimensions: i64,
        input: &str,
    ) -> Result<Vec<f32>, String>;
}

/// 生产 HTTP 实现：ureq + rustls（系统证书链），整体超时 30 秒。
pub struct UreqEmbeddingClient;

impl EmbeddingHttp for UreqEmbeddingClient {
    fn request_embedding(
        &self,
        base_url: &str,
        api_key: &str,
        model: &str,
        dimensions: i64,
        input: &str,
    ) -> Result<Vec<f32>, String> {
        let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
        let url = format!("{}/embeddings", base_url.trim().trim_end_matches('/'));
        let response = agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", api_key.trim()))
            .send_json(serde_json::json!({
                "model": model.trim(),
                "input": input,
                "dimensions": dimensions,
            }))
            .map_err(|error| format!("Embedding API 请求失败：{error}"))?;
        let document: serde_json::Value = response
            .into_json()
            .map_err(|error| format!("Embedding API 响应解析失败：{error}"))?;
        let embedding = document
            .get("data")
            .and_then(|data| data.get(0))
            .and_then(|item| item.get("embedding"))
            .and_then(|value| value.as_array())
            .ok_or_else(|| "Embedding API 响应缺少 data[0].embedding".to_string())?;
        embedding
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .map(|number| number as f32)
                    .ok_or_else(|| "Embedding API 响应向量元素非数字".to_string())
            })
            .collect()
    }
}

/// 缓存的查询向量与过期时刻。
struct CachedVector {
    vector: Vec<f32>,
    expires_at: DateTime<Utc>,
}

/// Embedding 应用服务。
pub struct EmbeddingService {
    database: Database,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    http: Arc<dyn EmbeddingHttp>,
    query_cache: Mutex<HashMap<String, CachedVector>>,
}

impl EmbeddingService {
    /// 创建 Embedding 服务；`http` 生产传 `UreqEmbeddingClient`。
    pub fn new(
        database: Database,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        http: Arc<dyn EmbeddingHttp>,
    ) -> Self {
        Self {
            database,
            clock,
            ids,
            http,
            query_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 返回配置视图（本地单机应用：API Key 明文回显，由界面端雾化展示）。
    pub fn get_settings(&self) -> Result<EmbeddingSettingsView, BusinessError> {
        let values = self.read_settings()?;
        let api_key = values.get("embedding.api_key").cloned().unwrap_or_default();
        let plain = if api_key.is_empty() {
            String::new()
        } else {
            memory_platform::dpapi::unprotect(&api_key)
                .map_err(|_| BusinessError::with_message(ErrorCode::InternalError, "Embedding API Key 解密失败"))?
        };
        Ok(EmbeddingSettingsView {
            base_url: values
                .get("embedding.base_url")
                .cloned()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            model: values
                .get("embedding.model")
                .cloned()
                .unwrap_or_else(|| "text-embedding-3-small".to_string()),
            api_key: plain,
            dimensions: values
                .get("embedding.dimensions")
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(1024),
            enabled: values
                .get("embedding.enabled")
                .is_some_and(|value| value.eq_ignore_ascii_case("true")),
            configured: !api_key.is_empty(),
        })
    }

    /// 测试配置并验证返回维度（文案与 C# `TestAsync` 逐字一致）。
    pub fn test(&self, request: &SaveEmbeddingSettingsRequest) -> Result<EmbeddingTestResult, BusinessError> {
        validate(request)?;
        let vector = self
            .http
            .request_embedding(
                &request.base_url,
                &request.api_key,
                &request.model,
                request.dimensions,
                "统一 AI 记忆连接测试",
            )
            .map_err(|message| BusinessError::with_message(ErrorCode::InternalError, message))?;
        if vector.len() as i64 != request.dimensions {
            return Ok(EmbeddingTestResult {
                success: false,
                dimensions: vector.len() as i64,
                message: format!(
                    "模型实际返回 {} 维，与配置的 {} 维不一致",
                    vector.len(),
                    request.dimensions
                ),
            });
        }
        Ok(EmbeddingTestResult {
            success: true,
            dimensions: vector.len() as i64,
            message: "Embedding API 连接成功".to_string(),
        })
    }

    /// 验证并保存配置：清空既有向量并入队全量重建（与 C# `SaveAsync` 一致）。
    pub fn save(&self, request: &SaveEmbeddingSettingsRequest) -> Result<EmbeddingSettingsView, BusinessError> {
        let test = self.test(request)?;
        if !test.success {
            return Err(BusinessError::with_message(
                ErrorCode::EmbeddingDimensionsMismatch,
                test.message,
            ));
        }
        let encrypted = memory_platform::dpapi::protect(request.api_key.trim())
            .map_err(|_| BusinessError::with_message(ErrorCode::InternalError, "Embedding API Key 加密失败"))?;
        let now_text = format_storage_time(self.clock.now_utc());
        let mut connection = self.database.open()?;
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        let values = [
            (
                "embedding.base_url",
                request.base_url.trim().trim_end_matches('/').to_string(),
            ),
            ("embedding.model", request.model.trim().to_string()),
            ("embedding.api_key", encrypted),
            ("embedding.dimensions", request.dimensions.to_string()),
            // 与 C# `bool.ToString()` 一致（存 "True"/"False"，bool.TryParse 双向可读）。
            (
                "embedding.enabled",
                if request.enabled { "True" } else { "False" }.to_string(),
            ),
        ];
        for (key, value) in values {
            transaction
                .execute(
                    "INSERT INTO app_setting(setting_key,setting_value,updated_at) \
                     VALUES($key,$value,$updated) \
                     ON CONFLICT(setting_key) DO UPDATE SET setting_value=excluded.setting_value,updated_at=excluded.updated_at;",
                    params![key, value, now_text],
                )
                .map_err(map_sqlite_error)?;
        }
        transaction
            .execute("DELETE FROM memory_embedding;", [])
            .map_err(map_sqlite_error)?;
        // 向量清空后：纯语义关系失去依据，一并删除；关键词关系保留待重算。
        transaction
            .execute("DELETE FROM memory_edge WHERE dominant_signal='SEMANTIC';", [])
            .map_err(map_sqlite_error)?;
        transaction.commit().map_err(map_sqlite_error)?;
        self.query_cache.lock().expect("Embedding 查询缓存已中毒").clear();
        self.queue_rebuild()?;
        // 按关键词重新计算全部图谱关系。
        self.queue_graph_rebuild()?;
        self.get_settings()
    }

    /// 排队一次全量图谱关系重建（向量档案更换后按当前可用信号重算）。
    fn queue_graph_rebuild(&self) -> Result<(), BusinessError> {
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        connection
            .execute(
                "DELETE FROM background_task WHERE task_type='REBUILD_GRAPH_ALL' AND status IN ('PENDING','RUNNING');",
                [],
            )
            .map_err(map_sqlite_error)?;
        connection
            .execute(
                "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
                 VALUES($id,'REBUILD_GRAPH_ALL',NULL,'PENDING',0,$next,NULL,NULL,$created,$updated);",
                params![self.ids.new_id(), now_text, now_text, now_text],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 创建一次全量向量重建任务（先清掉未完成的旧重建，与 C# 一致）。
    pub fn queue_rebuild(&self) -> Result<RebuildTicket, BusinessError> {
        let id = self.ids.new_id();
        let now_text = format_storage_time(self.clock.now_utc());
        let connection = self.database.open()?;
        connection
            .execute(
                "DELETE FROM background_task WHERE task_type='REBUILD_EMBEDDING' AND status IN ('PENDING','RUNNING');",
                [],
            )
            .map_err(map_sqlite_error)?;
        connection
            .execute(
                "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
                 VALUES($id,'REBUILD_EMBEDDING',NULL,'PENDING',0,$next,NULL,NULL,$created,$updated);",
                params![id, now_text, now_text, now_text],
            )
            .map_err(map_sqlite_error)?;
        Ok(RebuildTicket {
            id,
            status: "PENDING".to_string(),
        })
    }

    /// 返回向量配置与重建任务状态（对应 C# `GetStatusAsync` 匿名对象）。
    pub fn get_status(&self) -> Result<EmbeddingStatus, BusinessError> {
        let settings = self.get_settings()?;
        let connection = self.database.open()?;
        let pending: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type IN ('EMBED_MEMORY','REBUILD_EMBEDDING') AND status IN ('PENDING','RUNNING');",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        Ok(EmbeddingStatus {
            mode: if settings.enabled && settings.configured {
                "HYBRID".to_string()
            } else {
                "KEYWORD".to_string()
            },
            pending_tasks: pending,
        })
    }

    /// 使用已保存配置生成查询向量；配置不可用或请求失败返回 `None`
    /// （与 C# `TryCreateQueryVectorAsync` 吞掉所有非取消异常的语义一致）。
    pub fn try_create_query_vector(&self, input: &str) -> Option<Vec<f32>> {
        let view = self.get_settings().ok()?;
        if !view.enabled || !view.configured {
            return None;
        }
        let settings = self.read_settings().ok()?;
        let plain_key = memory_platform::dpapi::unprotect(settings.get("embedding.api_key")?).ok()?;
        let request = SaveEmbeddingSettingsRequest {
            base_url: view.base_url.clone(),
            model: view.model.clone(),
            api_key: plain_key,
            dimensions: view.dimensions,
            enabled: true,
        };
        let cache_key = create_cache_key(&request, input);
        let now = self.clock.now_utc();
        {
            let cache = self.query_cache.lock().expect("Embedding 查询缓存已中毒");
            if let Some(cached) = cache.get(&cache_key)
                && cached.expires_at > now
            {
                return Some(cached.vector.clone());
            }
        }
        let vector = normalize(
            self.http
                .request_embedding(
                    &request.base_url,
                    &request.api_key,
                    &request.model,
                    request.dimensions,
                    input,
                )
                .ok()?,
        );
        let mut cache = self.query_cache.lock().expect("Embedding 查询缓存已中毒");
        let expires_at = now + chrono::Duration::minutes(QUERY_CACHE_LIFETIME_MINUTES);
        cache.insert(
            cache_key,
            CachedVector {
                vector: vector.clone(),
                expires_at,
            },
        );
        // 写入时顺带清理过期条目（与 C# `RemoveExpiredCacheEntries` 一致）。
        cache.retain(|_, value| value.expires_at > now);
        Some(vector)
    }

    /// 为指定记忆生成并保存当前模型向量（与 C# `ProcessMemoryAsync` 一致）。
    ///
    /// 配置未启用、记忆不存在、不可云端处理或非 Active 时静默返回；
    /// HTTP 失败与维度不符返回错误，由后台任务按重试策略处理。
    pub fn process_memory(&self, memory_id: &str) -> Result<(), BusinessError> {
        let view = self.get_settings()?;
        if !view.enabled || !view.configured {
            return Ok(());
        }
        let settings = self.read_settings()?;
        let plain_key = memory_platform::dpapi::unprotect(
            settings
                .get("embedding.api_key")
                .ok_or_else(|| BusinessError::with_message(ErrorCode::InternalError, "Embedding API Key 缺失"))?,
        )
        .map_err(|_| BusinessError::with_message(ErrorCode::InternalError, "Embedding API Key 解密失败"))?;
        let request = SaveEmbeddingSettingsRequest {
            base_url: view.base_url.clone(),
            model: view.model.clone(),
            api_key: plain_key,
            dimensions: view.dimensions,
            enabled: true,
        };
        let connection = self.database.open()?;
        // 记忆行元组：title、summary、content、keywords、tags、checksum、云开关、状态。
        type MemoryRow = (String, String, String, String, String, String, bool, String);
        let row: Option<MemoryRow> = connection
            .query_row(
                "SELECT title,summary,content,keywords_json,tags_json,content_checksum,cloud_processing_allowed,status FROM memory WHERE id=$id;",
                params![memory_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        let Some((title, summary, content, keywords_json, tags_json, checksum, cloud_allowed, status)) = row else {
            return Ok(());
        };
        if !cloud_allowed || status != "Active" {
            return Ok(());
        }
        // 输入拼接顺序与 C# 一致：title、summary、keywords_json、tags_json、content。
        let input = [title, summary, keywords_json, tags_json, content].join("\n");
        let vector = normalize(
            self.http
                .request_embedding(
                    &request.base_url,
                    &request.api_key,
                    &request.model,
                    request.dimensions,
                    &input,
                )
                .map_err(|message| BusinessError::with_message(ErrorCode::InternalError, message))?,
        );
        if vector.len() as i64 != view.dimensions {
            return Err(BusinessError::new(ErrorCode::EmbeddingDimensionsMismatch));
        }
        let mut blob = Vec::with_capacity(vector.len() * 4);
        for value in &vector {
            blob.extend_from_slice(&value.to_le_bytes());
        }
        let now_text = format_storage_time(self.clock.now_utc());
        connection
            .execute(
                "INSERT INTO memory_embedding(memory_id,provider,model,dimensions,content_checksum,vector_blob,updated_at) \
                 VALUES($id,'OPENAI_COMPATIBLE',$model,$dimensions,$checksum,$vector,$updated) \
                 ON CONFLICT(memory_id) DO UPDATE SET provider=excluded.provider,model=excluded.model, \
                 dimensions=excluded.dimensions,content_checksum=excluded.content_checksum, \
                 vector_blob=excluded.vector_blob,updated_at=excluded.updated_at;",
                params![memory_id, view.model, view.dimensions, checksum, blob, now_text],
            )
            .map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 读取全部 Embedding 设置项。
    fn read_settings(&self) -> Result<HashMap<String, String>, BusinessError> {
        let connection = self.database.open()?;
        let mut statement = connection
            .prepare("SELECT setting_key,setting_value FROM app_setting WHERE setting_key LIKE 'embedding.%';")
            .map_err(map_sqlite_error)?;
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(map_sqlite_error)?;
        let mut values = HashMap::new();
        for row in rows {
            let (key, value) = row.map_err(map_sqlite_error)?;
            values.insert(key, value);
        }
        Ok(values)
    }
}

/// 校验模型配置（错误码与文案与 C# `Validate` 逐字一致）。
fn validate(request: &SaveEmbeddingSettingsRequest) -> Result<(), BusinessError> {
    let base_url = request.base_url.trim();
    // 近似 C# `Uri.TryCreate(Absolute)`：要求非空 scheme（http://、https:// 等）。
    let has_scheme = base_url
        .split_once("://")
        .is_some_and(|(scheme, _)| !scheme.is_empty() && !scheme.contains(char::is_whitespace));
    if !has_scheme {
        return Err(BusinessError::new(ErrorCode::EmbeddingUrlInvalid));
    }
    if request.model.trim().is_empty() || request.api_key.trim().is_empty() {
        return Err(BusinessError::new(ErrorCode::EmbeddingConfigIncomplete));
    }
    if !(128..=1536).contains(&request.dimensions) {
        return Err(BusinessError::new(ErrorCode::EmbeddingDimensionsInvalid));
    }
    Ok(())
}

/// 生成查询缓存键：SHA256 大写 hex（与 C# `CreateCacheKey` 的 material 与大小写一致）。
fn create_cache_key(request: &SaveEmbeddingSettingsRequest, input: &str) -> String {
    let material = format!(
        "{}\n{}\n{}\n{}",
        request.base_url, request.model, request.dimensions, input
    );
    Sha256::digest(material.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect()
}

/// 归一化向量（点积即余弦相似度）；零向量原样返回（与 C# `Normalize` 一致）。
fn normalize(mut vector: Vec<f32>) -> Vec<f32> {
    // 8 通道分块累加，对齐 C# `Vector<float>`（AVX2 下 Count=8）的求和方式。
    let mut lanes = [0.0f32; 8];
    let chunks = vector.chunks_exact(8);
    let remainder = chunks.remainder();
    for chunk in chunks {
        for (lane, value) in lanes.iter_mut().zip(chunk) {
            *lane += value * value;
        }
    }
    let mut sum: f32 = lanes.iter().sum();
    for value in remainder {
        sum += value * value;
    }
    let length = sum.sqrt();
    if length == 0.0 {
        return vector;
    }
    for value in &mut vector {
        *value /= length;
    }
    vector
}

/// 测试用固定向量 HTTP 桩（crate 内测试共享）。
#[cfg(test)]
pub(crate) struct StubEmbeddingHttp {
    pub dimensions: i64,
    pub fail: bool,
}

#[cfg(test)]
impl EmbeddingHttp for StubEmbeddingHttp {
    fn request_embedding(
        &self,
        _base_url: &str,
        _api_key: &str,
        _model: &str,
        _dimensions: i64,
        _input: &str,
    ) -> Result<Vec<f32>, String> {
        if self.fail {
            return Err("网络不可达".to_string());
        }
        Ok(vec![3.0; self.dimensions as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use crate::memory_service::MemoryService;
    use chrono::TimeZone;
    use memory_domain::{MemoryScope, SaveMemoryRequest};

    struct TestContext {
        #[allow(dead_code)] // 持有临时目录生命周期
        directory: tempfile::TempDir,
        database: Database,
        clock: Arc<FixedClock>,
        ids: Arc<FixedIdGenerator>,
    }

    fn context() -> TestContext {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("embedding.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let clock = Arc::new(FixedClock::new(
            chrono::Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap(),
        ));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=32)
                .map(|index| format!("{index:08}-0000-4000-8000-{index:012}"))
                .collect(),
        ));
        TestContext {
            directory,
            database: Database::new(database_path),
            clock,
            ids,
        }
    }

    fn make_service(context: &TestContext, dimensions: i64, fail: bool) -> EmbeddingService {
        EmbeddingService::new(
            context.database.clone(),
            context.clock.clone(),
            context.ids.clone(),
            Arc::new(StubEmbeddingHttp { dimensions, fail }),
        )
    }

    fn save_request() -> SaveEmbeddingSettingsRequest {
        SaveEmbeddingSettingsRequest {
            base_url: "https://api.example.com/v1".to_string(),
            model: "text-embedding-3-small".to_string(),
            api_key: "sk-test-key-123456".to_string(),
            dimensions: 128,
            enabled: true,
        }
    }

    #[test]
    fn default_settings_match_csharp_defaults() {
        let context = context();
        let service = make_service(&context, 128, false);
        let view = service.get_settings().unwrap();
        assert_eq!(view.base_url, "https://api.openai.com/v1");
        assert_eq!(view.model, "text-embedding-3-small");
        assert_eq!(view.dimensions, 1024);
        assert!(!view.enabled);
        assert!(!view.configured);
        assert_eq!(view.api_key, "");
        assert_eq!(service.get_status().unwrap().mode, "KEYWORD");
    }

    #[test]
    fn validate_rejects_invalid_requests() {
        let context = context();
        let service = make_service(&context, 128, false);
        let mut request = save_request();
        request.base_url = "not-a-url".to_string();
        assert_eq!(service.test(&request).unwrap_err().code, ErrorCode::EmbeddingUrlInvalid);
        let mut request = save_request();
        request.api_key = "  ".to_string();
        assert_eq!(
            service.test(&request).unwrap_err().code,
            ErrorCode::EmbeddingConfigIncomplete
        );
        let mut request = save_request();
        request.dimensions = 64;
        assert_eq!(
            service.test(&request).unwrap_err().code,
            ErrorCode::EmbeddingDimensionsInvalid
        );
    }

    #[test]
    fn test_reports_dimension_mismatch_message() {
        let context = context();
        // HTTP 桩固定返回 129 维，请求配置 128 维 → 不一致结果（不抛错）。
        let service = make_service(&context, 129, false);
        let result = service.test(&save_request()).unwrap();
        assert!(!result.success);
        assert_eq!(result.dimensions, 129);
        assert!(result.message.contains("129 维"));
        assert!(result.message.contains("128 维"));
    }

    #[test]
    fn save_persists_settings_clears_embeddings_and_queues_rebuild() {
        let context = context();
        let service = make_service(&context, 128, false);
        // 先建一条真实记忆（memory_embedding 有外键）并写入一条旧向量。
        let memories = MemoryService::new(context.database.clone(), context.clock.clone(), context.ids.clone());
        let memory = memories
            .create(&SaveMemoryRequest {
                scope: MemoryScope::Personal,
                project_id: None,
                title: "标题".to_string(),
                summary: "摘要".to_string(),
                content: "内容".to_string(),
                memory_type: "NOTE".to_string(),
                keywords: vec![],
                tags: vec![],
                importance: 3,
                is_favorite: false,
                is_pinned: false,
                cloud_processing_allowed: true,
                expected_version: None,
            })
            .unwrap();
        let connection = context.database.open().unwrap();
        connection
            .execute(
                "INSERT INTO memory_embedding(memory_id,provider,model,dimensions,content_checksum,vector_blob,updated_at) \
                 VALUES($id,'X','m',1,'c',x'00000000','2026-01-01T00:00:00.0000000+00:00');",
                params![memory.id],
            )
            .unwrap();
        drop(connection);
        let view = service.save(&save_request()).unwrap();
        assert!(view.configured);
        assert!(view.enabled);
        assert_eq!(view.dimensions, 128);
        assert_eq!(view.api_key, "sk-test-key-123456");
        let connection = context.database.open().unwrap();
        let embeddings: i64 = connection
            .query_row("SELECT count(*) FROM memory_embedding;", [], |row| row.get(0))
            .unwrap();
        let rebuilds: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='REBUILD_EMBEDDING' AND status='PENDING';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let stored_key: String = connection
            .query_row(
                "SELECT setting_value FROM app_setting WHERE setting_key='embedding.api_key';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let enabled: String = connection
            .query_row(
                "SELECT setting_value FROM app_setting WHERE setting_key='embedding.enabled';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        assert_eq!(embeddings, 0);
        assert_eq!(rebuilds, 1);
        assert_eq!(enabled, "True");
        // 存储的是 DPAPI 密文，且能被解回明文。
        assert_ne!(stored_key, "sk-test-key-123456");
        assert_eq!(
            memory_platform::dpapi::unprotect(&stored_key).unwrap(),
            "sk-test-key-123456"
        );
        assert_eq!(service.get_status().unwrap().mode, "HYBRID");
    }

    #[test]
    fn save_rejects_dimension_mismatch() {
        let context = context();
        let service = make_service(&context, 129, false);
        let error = service.save(&save_request()).unwrap_err();
        assert_eq!(error.code, ErrorCode::EmbeddingDimensionsMismatch);
        assert!(error.message.contains("129 维"));
    }

    #[test]
    fn query_vector_caches_and_fails_silently() {
        let context = context();
        let service = make_service(&context, 128, false);
        // 未配置 → None。
        assert!(service.try_create_query_vector("查询").is_none());
        service.save(&save_request()).unwrap();
        // 配置已就绪但 HTTP 失败 → 静默 None（save 会因 HTTP 失败报错，故只用查询路径验证）。
        let failing_service = make_service(&context, 128, true);
        assert!(failing_service.try_create_query_vector("查询").is_none());
        // 正常路径 → Some(归一化向量)，第二次命中缓存。
        let first = service.try_create_query_vector("查询").unwrap();
        assert_eq!(first.len(), 128);
        let norm: f32 = first.iter().map(|value| value * value).sum();
        assert!((norm - 1.0).abs() < 1e-5);
        let second = service.try_create_query_vector("查询").unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn cache_key_matches_csharp_material_format() {
        let request = save_request();
        let key = create_cache_key(&request, "输入");
        assert_eq!(key.len(), 64);
        assert!(
            key.chars()
                .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
        );
        let mut other = save_request();
        other.dimensions = 256;
        assert_ne!(key, create_cache_key(&other, "输入"));
    }

    #[test]
    fn normalize_handles_zero_and_scales() {
        assert_eq!(normalize(vec![0.0; 4]), vec![0.0; 4]);
        let normalized = normalize(vec![3.0, 4.0]);
        assert!((normalized[0] - 0.6).abs() < 1e-6);
        assert!((normalized[1] - 0.8).abs() < 1e-6);
        // 非 8 倍长度也正确。
        let normalized = normalize(vec![2.0; 13]);
        let sum: f32 = normalized.iter().map(|value| value * value).sum();
        assert!((sum - 1.0).abs() < 1e-5);
    }

    #[test]
    fn queue_rebuild_replaces_pending_duplicates() {
        let context = context();
        let service = make_service(&context, 128, false);
        service.queue_rebuild().unwrap();
        service.queue_rebuild().unwrap();
        let connection = context.database.open().unwrap();
        let rebuilds: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='REBUILD_EMBEDDING';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        assert_eq!(rebuilds, 1);
    }
}
