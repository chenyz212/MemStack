//! 中文 FTS5 检索、解释与上下文裁剪（C# `SearchService` 等价实现）。
//!
//! - 关键词召回：`memory_fts MATCH` + `bm25(1.0,5.0,4.0,3.0,1.0)`，字段权重
//!   title/keyword/summary/content = 5/4/3/1（首列为 unindexed memory_id，权重不生效）。
//! - 语义召回：`memory_embedding.vector_blob` 点积排序，仅保留相似度 ≥ 0.35 的候选
//!   （维度相等才参与），取前 100；低相似噪声不再进入结果。
//! - 排序：有效相似度降序——语义命中用余弦相似度（与卡片展示的匹配度同源，
//!   百分比随位次单调不增）；纯关键词命中用伪相似度（标题全等 0.90 /
//!   标题包含 0.75 / 其余 0.40）；同分依次按标题层级、关键词位次、
//!   语义位次、图谱位次、插入序决胜。
//! - 图谱扩展仅对已召回候选追加 GRAPH 原因与位次（作决胜项），不插入无关结果。
//! - FTS 零候选时模糊回退：LIKE 固定分 0.1、reason `["FUZZY"]`。
//! - 上下文：MMR（0.72 相关 − 0.28 多样）、字符二元组 Jaccard（≤500 对）、UTF-16 字符预算。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use memory_domain::{
    BusinessError, ContextEntry, ContextRequest, ContextResult, MemoryItem, SearchRequest, SearchResult,
};
use rusqlite::{Connection, params};

use crate::db::Database;
use crate::memory_service::{SELECT_MEMORY_SQL, read_memory};
use crate::sqlite_errors::map_sqlite_error;
use crate::tokenizer::{build_match_expression, tokenize};

/// 查询向量提供方抽象：生产包装 EmbeddingService，测试注入固定向量。
pub trait QueryVectorProvider: Send + Sync {
    /// 为查询生成向量；不可用返回 `None`（与 C# `TryCreateQueryVectorAsync` 语义一致）。
    fn try_create(&self, query: &str) -> Option<Vec<f32>>;
}

/// 生产实现：经 EmbeddingService（含 10 分钟查询缓存）。
pub struct EmbeddingQueryVectors {
    service: Arc<crate::embedding_service::EmbeddingService>,
}

impl EmbeddingQueryVectors {
    /// 包装 Embedding 服务。
    pub fn new(service: Arc<crate::embedding_service::EmbeddingService>) -> Self {
        Self { service }
    }
}

impl QueryVectorProvider for EmbeddingQueryVectors {
    fn try_create(&self, query: &str) -> Option<Vec<f32>> {
        self.service.try_create_query_vector(query)
    }
}

/// 融合候选：关键词、语义与图谱扩展三个召回位置。
struct RankedCandidate {
    memory: MemoryItem,
    keyword_rank: Option<i64>,
    semantic_rank: Option<i64>,
    graph_rank: Option<i64>,
    semantic_similarity: Option<f32>,
    reasons: Vec<String>,
}

/// 检索应用服务。
pub struct SearchService {
    database: Database,
    vectors: Arc<dyn QueryVectorProvider>,
}

impl SearchService {
    /// 创建检索服务；`vectors` 生产传 `EmbeddingQueryVectors`。
    pub fn new(database: Database, vectors: Arc<dyn QueryVectorProvider>) -> Self {
        Self { database, vectors }
    }

    /// 执行关键词检索，并在配置可用时标记混合模式（与 C# `SearchAsync` 一致）。
    pub fn search(&self, request: &SearchRequest) -> Result<Vec<SearchResult>, BusinessError> {
        let query = request.query.trim().to_string();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = request.limit.clamp(1, 100);
        let tokenized = tokenize(&query);
        if tokenized.is_empty() {
            return Ok(Vec::new());
        }
        let match_expression = build_match_expression(&tokenized);
        let query_vector = if request.semantic_enabled {
            self.vectors.try_create(&query)
        } else {
            None
        };
        let connection = self.database.open()?;

        // 关键词召回（FTS 序即 rank 序）。
        let keyword_memories = load_keyword_candidates(&connection, request, &match_expression, limit)?;
        // 候选集：HashMap 保内容，order 保 C# Dictionary 的插入序。
        let mut order: Vec<String> = Vec::new();
        let mut candidates: HashMap<String, RankedCandidate> = HashMap::new();
        for (position, memory) in keyword_memories.iter().enumerate() {
            let reasons = explain(memory, &query, false);
            order.push(memory.id.clone());
            candidates.insert(
                memory.id.clone(),
                RankedCandidate {
                    memory: memory.clone(),
                    keyword_rank: Some(position as i64 + 1),
                    semantic_rank: None,
                    graph_rank: None,
                    semantic_similarity: None,
                    reasons,
                },
            );
        }

        // 语义召回合并（仅当拿到查询向量），相似度随候选透传给调用方展示。
        if let Some(vector) = query_vector.as_ref() {
            let semantic = load_semantic_candidates(&connection, request, vector)?;
            for (index, (memory, similarity)) in semantic.iter().enumerate() {
                if let Some(current) = candidates.get_mut(&memory.id) {
                    current.semantic_rank = Some(index as i64 + 1);
                    current.semantic_similarity = Some(*similarity);
                    if !current.reasons.iter().any(|reason| reason == "SEMANTIC") {
                        current.reasons.push("SEMANTIC".to_string());
                    }
                } else {
                    order.push(memory.id.clone());
                    candidates.insert(
                        memory.id.clone(),
                        RankedCandidate {
                            memory: memory.clone(),
                            keyword_rank: None,
                            semantic_rank: Some(index as i64 + 1),
                            graph_rank: None,
                            semantic_similarity: Some(*similarity),
                            reasons: vec!["SEMANTIC".to_string()],
                        },
                    );
                }
            }
        }

        // 图谱一跳扩展：对强结果（召回位次前 5）沿 memory_edge 扩展（计划 §检索联动）。
        // 扩展邻居只给已召回候选追加 graph_rank 权重，不再以 GRAPH-only 身份
        // 插入与查询无关的结果（检索精度优先，扩展查询沿用同一范围过滤）。
        if !candidates.is_empty() {
            let seeds: Vec<String> = order.iter().take(5).cloned().collect();
            let graph = load_graph_neighbors(&connection, request, &seeds, &candidates)?;
            for (rank, (memory, _score)) in graph.iter().enumerate() {
                if let Some(current) = candidates.get_mut(&memory.id) {
                    current.graph_rank = Some(rank as i64 + 1);
                    if !current.reasons.iter().any(|reason| reason == "GRAPH") {
                        current.reasons.push("GRAPH".to_string());
                    }
                }
            }
        }

        if candidates.is_empty() {
            return fuzzy_fallback(&connection, request, &query, limit);
        }

        // 最终排序：有效相似度降序——与卡片展示的匹配度同源，
        // 保证展示百分比随位次单调不增（用户看到的第一名就是匹配度最高的）。
        // 同分依次按标题命中层级、关键词位次、语义位次、图谱位次、召回插入序决胜。
        let query_lower = query.to_lowercase();
        let mut ranked: Vec<(f64, u8, i64, i64, i64, usize, SearchResult)> = Vec::new();
        for (position, id) in order.iter().enumerate() {
            let candidate = &candidates[id];
            let title_lower = candidate.memory.title.to_lowercase();
            let title_exact = title_lower == query_lower;
            // 标题包含查询词（非全等）：无语义相似度时的强信号。
            let title_contains = !title_exact && title_lower.contains(&query_lower);
            let title_tier = if title_exact {
                2
            } else if title_contains {
                1
            } else {
                0
            };
            let score = effective_similarity(candidate.semantic_similarity, title_exact, title_contains);
            ranked.push((
                score,
                title_tier,
                candidate.keyword_rank.unwrap_or(i64::MAX),
                candidate.semantic_rank.unwrap_or(i64::MAX),
                candidate.graph_rank.unwrap_or(i64::MAX),
                position,
                SearchResult {
                    score,
                    memory: candidate.memory.clone(),
                    match_reasons: candidate.reasons.clone(),
                    semantic_similarity: candidate.semantic_similarity,
                },
            ));
        }
        ranked.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.1.cmp(&left.1))
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
                .then_with(|| left.4.cmp(&right.4))
                .then_with(|| left.5.cmp(&right.5))
        });
        let mut results: Vec<SearchResult> = ranked.into_iter().map(|entry| entry.6).collect();
        results.truncate(limit as usize);
        Ok(results)
    }

    /// 在严格字符预算内组装去重上下文（与 C# `BuildContextAsync` 一致）。
    pub fn build_context(&self, request: &ContextRequest) -> Result<ContextResult, BusinessError> {
        self.build_context_detailed(request).map(|(result, _)| result)
    }

    /// 组装上下文并返回入选记忆的作用域集合（供 MCP 活动文案判定 Mixed）。
    pub fn build_context_detailed(
        &self,
        request: &ContextRequest,
    ) -> Result<(ContextResult, std::collections::BTreeSet<String>), BusinessError> {
        let budget = request.max_characters.clamp(1, 30000);
        let mut search = request.search.clone();
        search.limit = 100;
        let results = self.search(&search)?;
        let mut entries: Vec<ContextEntry> = Vec::new();
        let mut scopes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut remaining: Vec<&SearchResult> = results.iter().collect();
        let mut used: i64 = 0;
        let mut truncated = false;
        while !remaining.is_empty() {
            let index = select_mmr_candidate(&remaining, &entries);
            let result = remaining.remove(index);
            let normalized = result.memory.content.trim();
            if !seen.insert(normalized.to_string()) {
                continue;
            }
            let fixed_length = utf16_length(&result.memory.title) + utf16_length(&result.memory.id) + 16;
            let available = budget - used - fixed_length;
            if available <= 0 {
                truncated = true;
                break;
            }
            let (content, content_truncated) = truncate_utf16(normalized, available as usize);
            truncated |= content_truncated;
            let entry = ContextEntry {
                id: result.memory.id.clone(),
                title: result.memory.title.clone(),
                content: content.clone(),
                match_reasons: result.match_reasons.clone(),
            };
            scopes.insert(result.memory.scope.as_scope_text().to_string());
            used += fixed_length + utf16_length(&content);
            entries.push(entry);
            if used >= budget {
                break;
            }
        }
        Ok((
            ContextResult {
                character_count: used.min(budget),
                items: entries,
                truncated,
            },
            scopes,
        ))
    }
}

/// 关键词候选：FTS 召回（含 bm25 权重与全部过滤参数）。
/// bm25 权重按列序对应 memory_id/title/keyword/summary/content；
/// 首列 memory_id 为 unindexed（权重不生效），实际生效 title/keyword/summary/content = 5/4/3/1。
fn load_keyword_candidates(
    connection: &Connection,
    request: &SearchRequest,
    match_expression: &str,
    limit: i64,
) -> Result<Vec<MemoryItem>, BusinessError> {
    let sql = "SELECT m.id,m.scope,m.project_id,p.name,m.title,m.summary,m.content,m.memory_type, \
               m.keywords_json,m.tags_json,m.importance,m.is_favorite,m.is_pinned,m.cloud_processing_allowed, \
               m.status,m.version,m.created_source,m.updated_source,m.created_at,m.updated_at,m.archived_at,rank \
               FROM memory_fts \
               JOIN memory m ON m.id=memory_fts.memory_id \
               LEFT JOIN project p ON p.id=m.project_id \
               WHERE memory_fts MATCH $query \
                 AND rank MATCH 'bm25(1.0,5.0,4.0,3.0,1.0)' \
                 AND m.status='Active' \
                 AND ($scope IS NULL OR m.scope=$scope) \
                 AND ($project_id IS NULL OR m.project_id=$project_id) \
                 AND ($type IS NULL OR m.memory_type=$type) \
                 AND ($tag IS NULL OR m.tags_json LIKE '%' || $tag || '%') \
               ORDER BY rank \
               LIMIT $limit;";
    let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(
            params![
                match_expression,
                request.scope.map(|scope| scope.as_scope_text().to_string()),
                request.project_id.clone(),
                normalized_filter(request.memory_type.as_deref()),
                normalized_filter(request.tag.as_deref()),
                limit
            ],
            read_memory,
        )
        .map_err(map_sqlite_error)?;
    let mut memories = Vec::new();
    for row in rows {
        memories.push(row.map_err(map_sqlite_error)?);
    }
    Ok(memories)
}

/// 语义候选最低相似度（向量已归一化，点积即余弦相似度）。
/// 低于该值的候选视为无关噪声，不进入结果——否则任意查询都会召回 Top-N。
const SEMANTIC_MIN_SIMILARITY: f32 = 0.35;

/// 语义候选：读取向量、过滤相似度 ≥ 阈值后按点积降序取前 100。
fn load_semantic_candidates(
    connection: &Connection,
    request: &SearchRequest,
    query_vector: &[f32],
) -> Result<Vec<(MemoryItem, f32)>, BusinessError> {
    let sql = "SELECT m.id,m.scope,m.project_id,p.name,m.title,m.summary,m.content,m.memory_type, \
               m.keywords_json,m.tags_json,m.importance,m.is_favorite,m.is_pinned,m.cloud_processing_allowed, \
               m.status,m.version,m.created_source,m.updated_source,m.created_at,m.updated_at,m.archived_at,e.vector_blob \
               FROM memory m LEFT JOIN project p ON p.id=m.project_id \
               JOIN memory_embedding e ON e.memory_id=m.id \
               WHERE m.status='Active' AND m.cloud_processing_allowed=1 \
                 AND ($scope IS NULL OR m.scope=$scope) \
                 AND ($project_id IS NULL OR m.project_id=$project_id) \
                 AND ($type IS NULL OR m.memory_type=$type) \
                 AND ($tag IS NULL OR m.tags_json LIKE '%' || $tag || '%');";
    let mut statement = connection.prepare(sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(
            params![
                request.scope.map(|scope| scope.as_scope_text().to_string()),
                request.project_id.clone(),
                normalized_filter(request.memory_type.as_deref()),
                normalized_filter(request.tag.as_deref())
            ],
            |row| {
                let memory = read_memory(row)?;
                let blob: Vec<u8> = row.get(21)?;
                Ok((memory, blob))
            },
        )
        .map_err(map_sqlite_error)?;
    let mut candidates: Vec<(MemoryItem, f32)> = Vec::new();
    for row in rows {
        let (memory, blob) = row.map_err(map_sqlite_error)?;
        if blob.len() % 4 != 0 {
            continue;
        }
        let vector: Vec<f32> = blob
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        if vector.len() == query_vector.len() {
            let similarity = dot_product(query_vector, &vector);
            if similarity >= SEMANTIC_MIN_SIMILARITY {
                candidates.push((memory, similarity));
            }
        }
    }
    // C# OrderByDescending 稳定排序后取前 100。
    candidates.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(100);
    Ok(candidates)
}

/// 模糊回退：FTS 零候选时 LIKE 匹配，分值 0.1、reason `["FUZZY"]`。
fn fuzzy_fallback(
    connection: &Connection,
    request: &SearchRequest,
    query: &str,
    limit: i64,
) -> Result<Vec<SearchResult>, BusinessError> {
    let sql = format!(
        "{SELECT_MEMORY_SQL} \
         WHERE m.status='Active' \
           AND ($scope IS NULL OR m.scope=$scope) \
           AND ($project_id IS NULL OR m.project_id=$project_id) \
           AND (m.title LIKE '%' || $query || '%' OR m.summary LIKE '%' || $query || '%' OR m.keywords_json LIKE '%' || $query || '%') \
         ORDER BY m.importance DESC,m.updated_at DESC LIMIT $limit;"
    );
    let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(
            params![
                request.scope.map(|scope| scope.as_scope_text().to_string()),
                request.project_id.clone(),
                query,
                limit
            ],
            read_memory,
        )
        .map_err(map_sqlite_error)?;
    let mut results = Vec::new();
    for row in rows {
        let memory = row.map_err(map_sqlite_error)?;
        results.push(SearchResult {
            memory,
            score: 0.1,
            match_reasons: vec!["FUZZY".to_string()],
            semantic_similarity: None,
        });
    }
    Ok(results)
}

/// 图谱一跳扩展：从强结果沿 memory_edge 找邻居，沿用检索的范围过滤，
/// 返回不在既有候选集中的记忆（按边分数降序，最多 8 条）。
fn load_graph_neighbors(
    connection: &Connection,
    request: &SearchRequest,
    seeds: &[String],
    candidates: &HashMap<String, RankedCandidate>,
) -> Result<Vec<(MemoryItem, f64)>, BusinessError> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    // 种子邻居：任一端为种子且分数达标。
    let placeholders = seeds.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let edge_sql = format!(
        "SELECT CASE WHEN memory_id_a IN ({placeholders}) THEN memory_id_b ELSE memory_id_a END AS other, \
         max(combined_score) AS best \
         FROM memory_edge WHERE combined_score >= 0.45 \
         AND (memory_id_a IN ({placeholders}) OR memory_id_b IN ({placeholders})) \
         GROUP BY other ORDER BY best DESC LIMIT 24;"
    );
    let mut edge_params: Vec<&str> = Vec::new();
    for _ in 0..3 {
        for seed in seeds {
            edge_params.push(seed.as_str());
        }
    }
    let mut edge_statement = connection.prepare(&edge_sql).map_err(map_sqlite_error)?;
    let edge_rows = edge_statement
        .query_map(rusqlite::params_from_iter(edge_params.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(map_sqlite_error)?;
    let mut neighbor_ids: Vec<String> = Vec::new();
    let mut best_scores: HashMap<String, f64> = HashMap::new();
    for row in edge_rows {
        let (id, score) = row.map_err(map_sqlite_error)?;
        if candidates.contains_key(&id) {
            continue;
        }
        best_scores.insert(id.clone(), score);
        neighbor_ids.push(id);
    }
    if neighbor_ids.is_empty() {
        return Ok(Vec::new());
    }
    // 邻居仍须满足检索范围过滤（项目 Token 不跨项目扩展）。
    let id_placeholders = neighbor_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let memory_sql = format!(
        "{SELECT_MEMORY_SQL} \
         WHERE m.status='Active' AND m.id IN ({id_placeholders}) \
           AND ($scope IS NULL OR m.scope=$scope) \
           AND ($project_id IS NULL OR m.project_id=$project_id) \
           AND ($type IS NULL OR m.memory_type=$type) \
           AND ($tag IS NULL OR m.tags_json LIKE '%' || $tag || '%');"
    );
    let scope_text = request.scope.map(|scope| scope.as_scope_text().to_string());
    let type_text = normalized_filter(request.memory_type.as_deref());
    let tag_text = normalized_filter(request.tag.as_deref());
    let mut memory_params: Vec<Option<&str>> = neighbor_ids.iter().map(|id| Some(id.as_str())).collect();
    memory_params.push(scope_text.as_deref());
    memory_params.push(request.project_id.as_deref());
    memory_params.push(type_text.as_deref());
    memory_params.push(tag_text.as_deref());
    let mut statement = connection.prepare(&memory_sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(memory_params), read_memory)
        .map_err(map_sqlite_error)?;
    let mut neighbors = Vec::new();
    for row in rows {
        let memory = row.map_err(map_sqlite_error)?;
        let score = *best_scores.get(&memory.id).unwrap_or(&0.0);
        neighbors.push((memory, score));
    }
    neighbors.sort_by(|left, right| right.1.partial_cmp(&left.1).unwrap_or(std::cmp::Ordering::Equal));
    neighbors.truncate(8);
    Ok(neighbors)
}

/// 过滤参数规范化：空白 → NULL，其余 trim（与 C# `IsNullOrWhiteSpace ? DBNull : Trim()` 一致）。
fn normalized_filter(value: Option<&str>) -> Option<String> {
    value
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_string())
}

/// 有效相似度（最终排序主键，与卡片展示的匹配度同源）。
/// 语义命中：余弦相似度原值（展示百分比即排序值，随位次单调不增）。
/// 纯关键词命中（无向量参与）：伪相似度与语义结果同量纲参与排序——
/// 标题全等 0.90 / 标题包含 0.75 / 其余 0.40（略高于语义召回阈值 0.35，
/// 词法命中是确定性信号，不应淹没在低相似语义结果之下）。
fn effective_similarity(similarity: Option<f32>, title_exact: bool, title_contains: bool) -> f64 {
    match similarity {
        Some(value) => value as f64,
        None if title_exact => 0.90,
        None if title_contains => 0.75,
        None => 0.40,
    }
}

/// 根据字段命中生成可解释原因（与 C# `Explain` 一致；忽略大小写比较）。
fn explain(memory: &MemoryItem, query: &str, semantic_available: bool) -> Vec<String> {
    let query_lower = query.to_lowercase();
    let title_lower = memory.title.to_lowercase();
    let mut reasons: Vec<String> = Vec::new();
    if title_lower == query_lower {
        reasons.push("TITLE_EXACT".to_string());
    } else if title_lower.starts_with(&query_lower) {
        reasons.push("TITLE_PREFIX".to_string());
    }
    if memory
        .keywords
        .iter()
        .any(|value| value.to_lowercase().contains(&query_lower))
    {
        reasons.push("KEYWORD".to_string());
    }
    if memory
        .tags
        .iter()
        .any(|value| value.to_lowercase().contains(&query_lower))
    {
        reasons.push("TAG".to_string());
    }
    if memory.summary.to_lowercase().contains(&query_lower) {
        reasons.push("SUMMARY".to_string());
    }
    if memory.content.to_lowercase().contains(&query_lower) {
        reasons.push("CONTENT".to_string());
    }
    if semantic_available {
        reasons.push("SEMANTIC".to_string());
    }
    if reasons.is_empty() {
        vec!["CONTENT".to_string()]
    } else {
        reasons
    }
}

/// 选择兼顾相关性和内容差异的下一个候选（与 C# `SelectMmrCandidate` 一致；
/// 严格大于保留首个最大，等价 C# OrderByDescending().First() 的稳定语义）。
fn select_mmr_candidate(candidates: &[&SearchResult], selected: &[ContextEntry]) -> usize {
    if selected.is_empty() {
        return 0;
    }
    let mut best_index = 0;
    let mut best_score = f64::NEG_INFINITY;
    for (index, candidate) in candidates.iter().enumerate() {
        let max_similarity = selected
            .iter()
            .map(|entry| text_similarity(&candidate.memory.content, &entry.content))
            .fold(f64::NEG_INFINITY, f64::max);
        let score = candidate.score * 0.72 - max_similarity * 0.28;
        if score > best_score {
            best_index = index;
            best_score = score;
        }
    }
    best_index
}

/// 字符二元集合估算内容相似度（Jaccard，与 C# `TextSimilarity` 一致）。
fn text_similarity(left: &str, right: &str) -> f64 {
    let left_pairs = create_pairs(left);
    let right_pairs = create_pairs(right);
    if left_pairs.is_empty() || right_pairs.is_empty() {
        return 0.0;
    }
    let intersection = left_pairs.iter().filter(|pair| right_pairs.contains(*pair)).count();
    intersection as f64 / (left_pairs.len() + right_pairs.len() - intersection) as f64
}

/// 创建至多五百个字符二元组（与 C# `CreatePairs` 一致：去空白、小写、相邻二元）。
fn create_pairs(value: &str) -> HashSet<String> {
    let compact: Vec<char> = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(|character| character.to_lowercase())
        .collect();
    let mut pairs = HashSet::new();
    let mut index = 0;
    while index + 1 < compact.len() && index < 500 {
        pairs.insert(compact[index..index + 2].iter().collect());
        index += 1;
    }
    pairs
}

/// 点积：8 通道分块累加（对齐 C# `Vector<float>` AVX2 求和方式）。
fn dot_product(left: &[f32], right: &[f32]) -> f32 {
    let mut lanes = [0.0f32; 8];
    let (chunks, remainder) = left.as_chunks::<8>();
    for (chunk_index, chunk) in chunks.iter().enumerate() {
        for (lane_index, value) in chunk.iter().enumerate() {
            lanes[lane_index] += value * right[chunk_index * 8 + lane_index];
        }
    }
    let mut sum: f32 = lanes.iter().sum();
    for (offset, value) in remainder.iter().enumerate() {
        sum += value * right[left.len() - remainder.len() + offset];
    }
    sum
}

/// UTF-16 code unit 长度（与 C# `string.Length` 一致）。
fn utf16_length(value: &str) -> i64 {
    value.chars().map(|character| character.len_utf16() as i64).sum()
}

/// 按 UTF-16 code unit 截断，返回（截断文本，是否发生截断）。
fn truncate_utf16(value: &str, max_units: usize) -> (String, bool) {
    let mut units = 0usize;
    let mut result = String::new();
    for character in value.chars() {
        let length = character.len_utf16();
        if units + length > max_units {
            return (result, true);
        }
        result.push(character);
        units += length;
    }
    (result, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use crate::memory_service::MemoryService;
    use chrono::TimeZone;
    use memory_domain::MemoryScope;
    use memory_domain::SaveMemoryRequest;

    /// 固定查询向量提供方。
    struct FixedVectors(Option<Vec<f32>>);

    impl QueryVectorProvider for FixedVectors {
        fn try_create(&self, _query: &str) -> Option<Vec<f32>> {
            self.0.clone()
        }
    }

    struct TestContext {
        #[allow(dead_code)]
        directory: tempfile::TempDir,
        database: Database,
        clock: Arc<FixedClock>,
        ids: Arc<FixedIdGenerator>,
        memories: Arc<MemoryService>,
    }

    fn context() -> TestContext {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("search.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let clock = Arc::new(FixedClock::new(
            chrono::Utc.with_ymd_and_hms(2026, 8, 15, 8, 0, 0).unwrap(),
        ));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=64)
                .map(|index| format!("{index:08}-0000-4000-8000-{index:012}"))
                .collect(),
        ));
        let database = Database::new(database_path);
        let memories = Arc::new(MemoryService::new(database.clone(), clock.clone(), ids.clone()));
        TestContext {
            directory,
            database,
            clock,
            ids,
            memories,
        }
    }

    fn save_request(
        title: &str,
        content: &str,
        keywords: Vec<&str>,
        tags: Vec<&str>,
        importance: i64,
    ) -> SaveMemoryRequest {
        SaveMemoryRequest {
            scope: MemoryScope::Personal,
            project_id: None,
            title: title.to_string(),
            summary: format!("{title} 的摘要"),
            content: content.to_string(),
            memory_type: "NOTE".to_string(),
            keywords: keywords.into_iter().map(String::from).collect(),
            tags: tags.into_iter().map(String::from).collect(),
            importance,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: true,
            expected_version: None,
        }
    }

    fn insert_embedding(context: &TestContext, memory_id: &str, vector: &[f32]) {
        let mut blob = Vec::with_capacity(vector.len() * 4);
        for value in vector {
            blob.extend_from_slice(&value.to_le_bytes());
        }
        context
            .database
            .open()
            .unwrap()
            .execute(
                "INSERT INTO memory_embedding(memory_id,provider,model,dimensions,content_checksum,vector_blob,updated_at) \
                 VALUES($id,'TEST','m',$dims,'c',$blob,'2026-08-15T08:00:00.0000000+00:00');",
                params![memory_id, vector.len() as i64, blob],
            )
            .unwrap();
    }

    fn search_request(query: &str) -> SearchRequest {
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
    fn search_returns_fts_hits_with_reasons() {
        let context = context();
        let first = context
            .memories
            .create(&save_request(
                "忆栈发布验证",
                "发布验证的内容正文",
                vec!["发布验证"],
                vec!["种子"],
                3,
            ))
            .unwrap();
        let second = context
            .memories
            .create(&save_request("另一个发布验证条目", "无关正文", vec!["发布"], vec![], 3))
            .unwrap();
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(None)));
        let results = service.search(&search_request("发布验证")).unwrap();
        assert!(!results.is_empty());
        let ids: Vec<&str> = results.iter().map(|result| result.memory.id.as_str()).collect();
        assert!(ids.contains(&first.id.as_str()));
        assert!(ids.contains(&second.id.as_str()));
        // 标题命中或关键词命中应有可解释原因。
        let first_result = results.iter().find(|result| result.memory.id == first.id).unwrap();
        assert!(first_result.match_reasons.iter().any(|reason| {
            reason == "TITLE_PREFIX" || reason == "KEYWORD" || reason == "CONTENT" || reason == "SUMMARY"
        }));
        // score 为正（keyword RRF + metadata）。
        assert!(first_result.score > 0.0);
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let context = context();
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(None)));
        assert!(service.search(&search_request("   ")).unwrap().is_empty());
    }

    #[test]
    fn fuzzy_fallback_triggers_on_fts_miss() {
        let context = context();
        let memory = context
            .memories
            .create(&save_request("ABCDEF 模糊样本", "完全无关正文", vec![], vec![], 3))
            .unwrap();
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(None)));
        // "bcd" 不是 "abcdef" 的前缀 → FTS 未命中；LIKE '%bcd%' 命中标题。
        let results = service.search(&search_request("bcd")).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.id, memory.id);
        assert_eq!(results[0].score, 0.1);
        assert_eq!(results[0].match_reasons, vec!["FUZZY".to_string()]);
    }

    #[test]
    fn title_exact_boosts_score_to_top() {
        let context = context();
        let exact = context
            .memories
            .create(&save_request("忆栈", "标题精确命中的正文", vec!["忆栈"], vec![], 3))
            .unwrap();
        let other = context
            .memories
            .create(&save_request("忆栈备选条目", "备选条目的正文", vec!["忆栈"], vec![], 5))
            .unwrap();
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(None)));
        let results = service.search(&search_request("忆栈")).unwrap();
        assert!(results.len() >= 2);
        assert_eq!(results[0].memory.id, exact.id);
        // 无语义命中：标题全等伪相似度 0.90；标题包含 0.75 次之。
        assert_eq!(results[0].score, 0.90);
        assert!(results[0].match_reasons.contains(&"TITLE_EXACT".to_string()));
        assert!(results.iter().any(|result| result.memory.id == other.id));
    }

    #[test]
    fn title_contains_boosts_partial_title_to_top() {
        let context = context();
        // 搜"发布流程"：标题包含它的记忆应排第一，正文提到它的排后面。
        let titled = context
            .memories
            .create(&save_request("团队发布流程规范", "规范说明", vec![], vec![], 3))
            .unwrap();
        let mentioned = context
            .memories
            .create(&save_request("周报", "本周整理了发布流程相关的进展", vec![], vec![], 5))
            .unwrap();
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(None)));
        let results = service.search(&search_request("发布流程")).unwrap();
        assert!(results.len() >= 2);
        assert_eq!(results[0].memory.id, titled.id);
        assert!(results.iter().any(|result| result.memory.id == mentioned.id));
        // 标题包含伪相似度 0.75，明显高于正文命中（0.40）。
        assert_eq!(results[0].score, 0.75);
    }

    #[test]
    fn semantic_below_threshold_returns_nothing() {
        let context = context();
        // FTS 无法命中（标题/正文都不含查询词），语义相似度 0.2 < 0.35 阈值。
        let memory = context
            .memories
            .create(&save_request("无关条目", "无关正文", vec![], vec![], 3))
            .unwrap();
        insert_embedding(&context, &memory.id, &[0.2, 0.98, 0.0, 0.0]);
        let service = SearchService::new(
            context.database.clone(),
            Arc::new(FixedVectors(Some(vec![1.0, 0.0, 0.0, 0.0]))),
        );
        let mut request = search_request("毫不相干的查询xyz");
        request.semantic_enabled = true;
        let results = service.search(&request).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn semantic_merge_adds_rank_and_reason() {
        let context = context();
        let keyword_hit = context
            .memories
            .create(&save_request(
                "语义测试条目",
                "语义测试正文",
                vec!["语义测试"],
                vec![],
                3,
            ))
            .unwrap();
        let semantic_only = context
            .memories
            .create(&save_request("完全不相关标题", "完全不相关正文内容", vec![], vec![], 3))
            .unwrap();
        // 4 维查询向量；语义记忆相似度高，关键词记忆相似度中等（均过 0.35 阈值）。
        let query_vector = vec![1.0, 0.0, 0.0, 0.0];
        insert_embedding(&context, &keyword_hit.id, &[0.4, 0.3, 0.0, 0.0]);
        insert_embedding(&context, &semantic_only.id, &[0.9, 0.1, 0.0, 0.0]);
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(Some(query_vector))));
        let mut request = search_request("语义测试");
        request.semantic_enabled = true;
        let results = service.search(&request).unwrap();
        // 相似度高者排第一（有效相似度即排序主键）。
        assert_eq!(results[0].memory.id, semantic_only.id);
        // 语义记忆进入结果且 reason 仅为 SEMANTIC。
        let semantic_result = results
            .iter()
            .find(|result| result.memory.id == semantic_only.id)
            .expect("语义召回条目应出现在结果中");
        assert_eq!(semantic_result.match_reasons, vec!["SEMANTIC".to_string()]);
        // 相似度透传（点积 0.9），排序值与展示值一致。
        assert!((semantic_result.semantic_similarity.unwrap() - 0.9).abs() < 1e-5);
        assert!((semantic_result.score - 0.9).abs() < 1e-5);
        // 关键词条目追加 SEMANTIC（去重），相似度同样透传（点积 0.4）。
        let keyword_result = results
            .iter()
            .find(|result| result.memory.id == keyword_hit.id)
            .expect("关键词条目应在结果中");
        assert!(keyword_result.match_reasons.contains(&"SEMANTIC".to_string()));
        assert!((keyword_result.semantic_similarity.unwrap() - 0.4).abs() < 1e-5);
        assert!((keyword_result.score - 0.4).abs() < 1e-5);
    }

    #[test]
    fn semantic_dimension_mismatch_is_skipped() {
        let context = context();
        let memory = context
            .memories
            .create(&save_request("维度不符条目", "维度不符正文", vec![], vec![], 3))
            .unwrap();
        insert_embedding(&context, &memory.id, &[1.0, 0.0, 0.0]); // 3 维 vs 查询 4 维
        let service = SearchService::new(
            context.database.clone(),
            Arc::new(FixedVectors(Some(vec![1.0, 0.0, 0.0, 0.0]))),
        );
        let mut request = search_request("不存在的查询词zzz");
        request.semantic_enabled = true;
        let results = service.search(&request).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn context_respects_budget_and_dedupes() {
        let context = context();
        let long_content = "记忆正文重复内容。".repeat(50);
        context
            .memories
            .create(&save_request("上下文甲", &long_content, vec!["上下文"], vec![], 3))
            .unwrap();
        // 同正文放项目范围（库级去重按 scope+内容），验证上下文层内容去重。
        let projects = Arc::new(crate::project_service::ProjectService::new(
            context.database.clone(),
            context.clock.clone(),
            context.ids.clone(),
        ));
        let project = projects
            .create(&memory_domain::SaveProjectRequest {
                name: "上下文项目".to_string(),
                description: String::new(),
                color: "#1890ff".to_string(),
            })
            .unwrap();
        context
            .memories
            .create(&SaveMemoryRequest {
                scope: MemoryScope::Project,
                project_id: Some(project.id.clone()),
                title: "上下文乙".to_string(),
                summary: "上下文乙 的摘要".to_string(),
                content: long_content.clone(),
                memory_type: "NOTE".to_string(),
                keywords: vec!["上下文".to_string()],
                tags: vec![],
                importance: 3,
                is_favorite: false,
                is_pinned: false,
                cloud_processing_allowed: false,
                expected_version: None,
            })
            .unwrap();
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(None)));
        let result = service
            .build_context(&ContextRequest {
                search: search_request("上下文"),
                max_characters: 300,
            })
            .unwrap();
        // 去重后仅 1 条；预算被尊重且发生截断。
        assert_eq!(result.items.len(), 1);
        assert!(result.truncated);
        assert!(result.character_count <= 300);
        assert!(utf16_length(&result.items[0].content) <= 300);
    }

    #[test]
    fn context_includes_full_content_within_large_budget() {
        let context = context();
        context
            .memories
            .create(&save_request("完整上下文", "短正文", vec!["完整"], vec![], 3))
            .unwrap();
        let service = SearchService::new(context.database.clone(), Arc::new(FixedVectors(None)));
        let result = service
            .build_context(&ContextRequest {
                search: search_request("完整"),
                max_characters: 30000,
            })
            .unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].content, "短正文");
        assert!(!result.truncated);
    }

    #[test]
    fn effective_similarity_tiers_match_display_order() {
        // 语义命中：相似度原值即排序值（展示百分比与位次一致）。
        assert!((effective_similarity(Some(0.62), false, false) - 0.62).abs() < 1e-6);
        assert!((effective_similarity(Some(0.9), true, false) - 0.9).abs() < 1e-6);
        // 纯关键词命中：伪相似度同量纲参与排序。
        assert_eq!(effective_similarity(None, true, false), 0.90);
        assert_eq!(effective_similarity(None, false, true), 0.75);
        assert_eq!(effective_similarity(None, false, false), 0.40);
    }

    #[test]
    fn semantic_results_sorted_by_similarity_desc() {
        let context = context();
        // 三条语义候选（相似度 0.5/0.9/0.6），FTS 均不命中。
        let low = context
            .memories
            .create(&save_request("甲条目", "甲正文", vec![], vec![], 3))
            .unwrap();
        let high = context
            .memories
            .create(&save_request("乙条目", "乙正文", vec![], vec![], 3))
            .unwrap();
        let mid = context
            .memories
            .create(&save_request("丙条目", "丙正文", vec![], vec![], 3))
            .unwrap();
        insert_embedding(&context, &low.id, &[0.5, 0.866, 0.0, 0.0]);
        insert_embedding(&context, &high.id, &[0.9, 0.436, 0.0, 0.0]);
        insert_embedding(&context, &mid.id, &[0.6, 0.8, 0.0, 0.0]);
        let service = SearchService::new(
            context.database.clone(),
            Arc::new(FixedVectors(Some(vec![1.0, 0.0, 0.0, 0.0]))),
        );
        let mut request = search_request("毫不相干查询xyz");
        request.semantic_enabled = true;
        let results = service.search(&request).unwrap();
        assert_eq!(results.len(), 3);
        // 匹配度最高的排第一，展示值随位次单调不增。
        assert_eq!(results[0].memory.id, high.id);
        assert_eq!(results[1].memory.id, mid.id);
        assert_eq!(results[2].memory.id, low.id);
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
    }

    #[test]
    fn dot_product_matches_scalar_reference() {
        let left = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 0.5, -0.5];
        let right = vec![0.5, -1.0, 2.0, 0.0, 1.0, 1.0, -2.0, 3.0, 4.0, 0.25];
        let expected: f32 = left.iter().zip(&right).map(|(l, r)| l * r).sum();
        assert!((dot_product(&left, &right) - expected).abs() < 1e-5);
    }

    #[test]
    fn mmr_prefers_diverse_candidate_over_near_duplicate() {
        let make = |id: &str, score: f64, content: &str| SearchResult {
            memory: MemoryItem {
                id: id.to_string(),
                scope: MemoryScope::Personal,
                project_id: None,
                project_name: None,
                title: format!("标题{id}"),
                summary: String::new(),
                content: content.to_string(),
                memory_type: "NOTE".to_string(),
                keywords: vec![],
                tags: vec![],
                importance: 3,
                is_favorite: false,
                is_pinned: false,
                cloud_processing_allowed: false,
                status: memory_domain::MemoryStatus::Active,
                version: 1,
                created_source: "桌面".to_string(),
                updated_source: "桌面".to_string(),
                created_at: "2026-08-15T08:00:00.0000000+00:00".to_string(),
                updated_at: "2026-08-15T08:00:00.0000000+00:00".to_string(),
                archived_at: None,
            },
            score,
            match_reasons: vec!["CONTENT".to_string()],
            semantic_similarity: None,
        };
        let reference = make("a", 0.02, "甲乙丙丁戊己庚辛");
        let near_duplicate = make("b", 0.019, "甲乙丙丁戊己庚辛");
        let diverse = make("c", 0.018, "子丑寅卯辰巳午未");
        let candidates = vec![&reference, &near_duplicate, &diverse];
        // 已选 reference：近重复被多样性惩罚，多样候选胜出。
        let selected = vec![ContextEntry {
            id: "a".to_string(),
            title: "标题a".to_string(),
            content: "甲乙丙丁戊己庚辛".to_string(),
            match_reasons: vec![],
        }];
        let index = select_mmr_candidate(&candidates, &selected);
        assert_eq!(candidates[index].memory.id, "c");
    }
}
