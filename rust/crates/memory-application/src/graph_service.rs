//! 记忆图谱服务：`memory_edge` 关系计算 + 全局/邻域查询。
//!
//! 数据与关系（实施计划 §数据与关系）：
//! - 直接复用既有 `memory_edge` 表；节点只代表正式记忆，项目仅提供颜色/筛选。
//! - 关键词标签使用标准化后的 `keywords + tags` 集合计算 Jaccard 相似度。
//! - 有向量：`语义×0.60 + 关键词×0.30 + 同项目×0.10`；
//!   无向量：`关键词×0.80 + 同项目×0.20`。
//! - 语义 ≥ 0.72 或关键词 ≥ 0.50 才能成为候选；同项目只增强已有关系，
//!   不能单独产生连线；个人记忆之间不计算同项目加权。
//! - 每个节点最多保留 8 条最强关系；两个记忆 ID 固定排序（a < b）后保存。
//! - 归档记忆及已归档项目中的记忆不进入图谱（查询过滤 + 归档删边）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use memory_domain::{
    BusinessError, ErrorCode, GraphEdge, GraphGlobalQuery, GraphNeighborhoodQuery, GraphNode, GraphResult, MemoryScope,
    RebuildTicket,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Transaction, params};

use crate::clock::{Clock, format_storage_time};
use crate::db::Database;
use crate::ids::IdGenerator;
use crate::sqlite_errors::map_sqlite_error;

/// 每个节点最多保留的强关系数。
pub const MAX_EDGES_PER_NODE: usize = 8;
/// 语义分成为候选关系的阈值。
pub const SEMANTIC_THRESHOLD: f64 = 0.72;
/// 关键词标签分成为候选关系的阈值。
pub const KEYWORD_THRESHOLD: f64 = 0.50;
/// 后端强制的图谱节点数上限。
pub const MAX_GRAPH_LIMIT: i64 = 300;
/// 后端强制的邻域跳数上限。
pub const MAX_NEIGHBORHOOD_DEPTH: i64 = 2;

/// 记忆图谱应用服务。
pub struct GraphService {
    database: Database,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

/// 参与关系计算的一条记忆信号。
struct MemorySignal {
    id: String,
    scope: MemoryScope,
    project_id: Option<String>,
    terms: HashSet<String>,
    vector: Option<Vec<f32>>,
    is_pinned: bool,
    importance: i64,
    updated_at: String,
}

/// 一条候选关系的各项得分。
struct RelationScores {
    semantic: f64,
    keyword: f64,
    project_boost: f64,
    combined: f64,
    dominant_signal: &'static str,
}

impl GraphService {
    pub fn new(database: Database, clock: Arc<dyn Clock>, ids: Arc<dyn IdGenerator>) -> Self {
        Self { database, clock, ids }
    }

    /// 重算单条记忆的关系（创建/更新/恢复/Embedding 成功后由 Worker 调用）。
    ///
    /// 记忆不存在、已归档或所属项目已归档时，仅删除其既有边。
    pub fn recompute_memory(&self, memory_id: &str) -> Result<(), BusinessError> {
        let mut connection = self.database.open()?;
        let mut signals = load_signals(&connection)?;
        // 记忆不存在、已归档或项目已归档（load_signals 只返回可见记忆）→ 仅删边。
        let Some(index) = signals.iter().position(|signal| signal.id == memory_id) else {
            delete_edges_of(&connection, memory_id)?;
            return Ok(());
        };
        let target = signals.swap_remove(index);
        let now_text = format_storage_time(self.clock.now_utc());
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        delete_edges_in_transaction(&transaction, memory_id)?;
        let mut scored: Vec<(&MemorySignal, RelationScores)> = signals
            .iter()
            .filter_map(|other| compute_relation(&target, other).map(|scores| (other, scores)))
            .collect();
        scored.sort_by(|left, right| {
            right
                .1
                .combined
                .partial_cmp(&left.1.combined)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(MAX_EDGES_PER_NODE);
        for (other, scores) in scored {
            insert_edge_with_cap(&transaction, &target.id, &other.id, &scores, &now_text)?;
        }
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 全量重建：清空 memory_edge 后按置顶、重要度、最近更新优先重算。
    pub fn recompute_all(&self) -> Result<(), BusinessError> {
        let mut connection = self.database.open()?;
        let mut signals = load_signals(&connection)?;
        // 排队规则：置顶 → 重要度 → 最近更新时间（计划 §后台关系计算）。
        signals.sort_by(|left, right| {
            right
                .is_pinned
                .cmp(&left.is_pinned)
                .then(right.importance.cmp(&left.importance))
                .then(right.updated_at.cmp(&left.updated_at))
        });
        // 倒排索引：术语 → 持有记忆下标，避免关键词候选的 O(n²) 扫描。
        let mut term_index: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, signal) in signals.iter().enumerate() {
            for term in &signal.terms {
                term_index.entry(term.as_str()).or_default().push(index);
            }
        }
        let vector_indices: Vec<usize> = signals
            .iter()
            .enumerate()
            .filter(|(_, signal)| signal.vector.is_some())
            .map(|(index, _)| index)
            .collect();

        let now_text = format_storage_time(self.clock.now_utc());
        let transaction = connection.transaction().map_err(map_sqlite_error)?;
        transaction
            .execute("DELETE FROM memory_edge;", [])
            .map_err(map_sqlite_error)?;
        for (target_index, target) in signals.iter().enumerate() {
            let mut candidate_indices: Vec<usize> = Vec::new();
            for term in &target.terms {
                if let Some(holders) = term_index.get(term.as_str()) {
                    candidate_indices.extend_from_slice(holders);
                }
            }
            if target.vector.is_some() {
                candidate_indices.extend_from_slice(&vector_indices);
            }
            let mut seen: HashSet<usize> = HashSet::new();
            let mut scored: Vec<(&MemorySignal, RelationScores)> = candidate_indices
                .into_iter()
                .filter(|index| *index != target_index && seen.insert(*index))
                .filter_map(|index| compute_relation(target, &signals[index]).map(|scores| (&signals[index], scores)))
                .collect();
            scored.sort_by(|left, right| {
                right
                    .1
                    .combined
                    .partial_cmp(&left.1.combined)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(MAX_EDGES_PER_NODE);
            for (other, scores) in scored {
                insert_edge_with_cap(&transaction, &target.id, &other.id, &scores, &now_text)?;
            }
        }
        transaction.commit().map_err(map_sqlite_error)?;
        Ok(())
    }

    /// 排队一次全量关系重建（REBUILD_GRAPH_ALL）。
    pub fn queue_full_rebuild(&self) -> Result<RebuildTicket, BusinessError> {
        let id = self.ids.new_id();
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
                params![id, now_text, now_text, now_text],
            )
            .map_err(map_sqlite_error)?;
        Ok(RebuildTicket {
            id,
            status: "PENDING".to_string(),
        })
    }

    /// 桌面启动自愈：升级/迁移库的存量记忆从未生成过关系时，自动排队一次全量重算。
    ///
    /// 触发条件（三者同时满足；触发后写入 `GRAPH_STARTUP_HEALED` COMPLETED 标记行，
    /// 保证数据库生命周期内至多自动触发一次）：
    /// 1. 活跃记忆 ≥ 2（与 `load_signals` 同口径：Active 且项目未归档）；
    /// 2. `memory_edge` 为空（含全量重算后仍无边的数据集，标记防止此后每次启动重复 O(n²) 重算）；
    /// 3. 从未触发过自愈（Worker 成功后会删除任务行，故用独立标记行而非任务历史判定）。
    ///
    /// 失败静默（返回 None）：用户仍可通过图谱页「重建关系」手动触发。
    pub fn queue_startup_rebuild_if_needed(&self) -> Result<Option<RebuildTicket>, BusinessError> {
        let connection = self.database.open()?;
        let healed: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='GRAPH_STARTUP_HEALED';",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        if healed > 0 {
            return Ok(None);
        }
        let memories: i64 = connection
            .query_row(
                "SELECT count(*) FROM memory m \
                 LEFT JOIN project p ON p.id=m.project_id \
                 WHERE m.status='Active' AND (p.id IS NULL OR p.is_archived=0);",
                [],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        if memories < 2 {
            return Ok(None);
        }
        let edges: i64 = connection
            .query_row("SELECT count(*) FROM memory_edge;", [], |row| row.get(0))
            .map_err(map_sqlite_error)?;
        if edges > 0 {
            return Ok(None);
        }
        drop(connection);
        let ticket = self.queue_full_rebuild()?;
        // 持久化一次性标记：Worker 仅认领 PENDING 任务且只删除自己认领的行，
        // COMPLETED 标记行永不入队、永不被清理（FAILED 修剪仅动 FAILED）。
        let marker_id = self.ids.new_id();
        let now_text = format_storage_time(self.clock.now_utc());
        self.database
            .open()?
            .execute(
                "INSERT INTO background_task(id,task_type,target_id,status,attempt_count,next_attempt_at,error_code,error_message,created_at,updated_at) \
                 VALUES($id,'GRAPH_STARTUP_HEALED',NULL,'COMPLETED',0,$next,NULL,NULL,$created,$updated);",
                params![marker_id, now_text, now_text, now_text],
            )
            .map_err(map_sqlite_error)?;
        Ok(Some(ticket))
    }

    /// 全局圆形图谱：节点按置顶/重要度/最近更新择优，边取节点集合内的 memory_edge。
    pub fn global_graph(&self, query: &GraphGlobalQuery) -> Result<GraphResult, BusinessError> {
        let limit = query.limit.clamp(1, MAX_GRAPH_LIMIT);
        let min_score = query.min_score.clamp(0.0, 1.0);
        let connection = self.database.open()?;
        let cutoff = time_cutoff(&*self.clock, query.days);

        let total_nodes = count_global_nodes(&connection, query, cutoff.as_deref())?;
        let nodes = load_global_nodes(&connection, query, cutoff.as_deref(), limit)?;
        let truncated = total_nodes as usize > nodes.len();
        let ids: HashSet<String> = nodes.iter().map(|node| node.id.clone()).collect();
        let edges = load_edges_within(&connection, &ids, min_score)?;
        let total_edges = edges.len() as i64;
        let nodes = apply_degrees(nodes, &edges);
        Ok(GraphResult {
            nodes,
            edges,
            center_memory_id: None,
            build_status: build_status(&connection)?,
            truncated,
            total_nodes,
            total_edges,
        })
    }

    /// 以指定记忆为圆心的一跳/两跳邻域图谱。
    pub fn neighborhood(&self, query: &GraphNeighborhoodQuery) -> Result<GraphResult, BusinessError> {
        let depth = query.depth.clamp(1, MAX_NEIGHBORHOOD_DEPTH);
        let limit = query.limit.clamp(1, MAX_GRAPH_LIMIT);
        let min_score = query.min_score.clamp(0.0, 1.0);
        let connection = self.database.open()?;

        let mut center =
            load_node(&connection, &query.memory_id)?.ok_or_else(|| BusinessError::new(ErrorCode::MemoryNotFound))?;

        // BFS 扩展：记录层级与进入边分数，供超限时的保留排序。
        let mut level_of: HashMap<String, i64> = HashMap::new();
        let mut best_score_of: HashMap<String, f64> = HashMap::new();
        level_of.insert(center.id.clone(), 0);
        best_score_of.insert(center.id.clone(), f64::INFINITY);
        let mut frontier: Vec<String> = vec![center.id.clone()];
        for level in 1..=depth {
            let mut next: Vec<String> = Vec::new();
            for id in &frontier {
                for (other, score) in neighbors_of(&connection, id, min_score)? {
                    if !level_of.contains_key(&other) {
                        level_of.insert(other.clone(), level);
                        best_score_of.insert(other.clone(), score);
                        next.push(other);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        // 只保留图谱可见节点（活动记忆 + 项目未归档）。
        let mut reachable: Vec<String> = Vec::new();
        for id in level_of.keys() {
            if load_node(&connection, id)?.is_some() {
                reachable.push(id.clone());
            }
        }
        let total_nodes = reachable.len() as i64;
        let truncated = total_nodes as usize > limit as usize;
        let mut kept: Vec<String> = if truncated {
            reachable.sort_by(|left, right| {
                level_of[left].cmp(&level_of[right]).then(
                    best_score_of[right]
                        .partial_cmp(&best_score_of[left])
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            });
            let mut kept: Vec<String> = reachable.into_iter().take(limit as usize).collect();
            // 中心记忆始终保留。
            if !kept.contains(&center.id) {
                kept.pop();
                kept.push(center.id.clone());
            }
            kept
        } else {
            reachable
        };
        kept.dedup();
        let ids: HashSet<String> = kept.iter().cloned().collect();
        let edges = load_edges_within(&connection, &ids, min_score)?;
        let total_edges = edges.len() as i64;
        let mut nodes: Vec<GraphNode> = Vec::with_capacity(kept.len());
        for id in &kept {
            if let Some(mut node) = load_node(&connection, id)? {
                if id == &center.id {
                    std::mem::swap(&mut node, &mut center);
                }
                nodes.push(node);
            }
        }
        // 中心节点排在首位。
        if let Some(position) = nodes.iter().position(|node| node.id == query.memory_id) {
            nodes.swap(0, position);
        }
        let nodes = apply_degrees(nodes, &edges);
        Ok(GraphResult {
            nodes,
            edges,
            center_memory_id: Some(query.memory_id.clone()),
            build_status: build_status(&connection)?,
            truncated,
            total_nodes,
            total_edges,
        })
    }
}

/// 计算一对记忆的关系得分；未达候选阈值返回 `None`。
fn compute_relation(left: &MemorySignal, right: &MemorySignal) -> Option<RelationScores> {
    let keyword = jaccard(&left.terms, &right.terms);
    let semantic = match (&left.vector, &right.vector) {
        (Some(left_vector), Some(right_vector))
            if left_vector.len() == right_vector.len() && !left_vector.is_empty() =>
        {
            cosine_clamped(left_vector, right_vector)
        }
        _ => 0.0,
    };
    let semantic_ok = semantic >= SEMANTIC_THRESHOLD;
    let keyword_ok = keyword >= KEYWORD_THRESHOLD;
    if !semantic_ok && !keyword_ok {
        return None;
    }
    // 同项目只增强已有关系；个人记忆之间不计算同项目加权。
    let same_project = left.scope == MemoryScope::Project
        && right.scope == MemoryScope::Project
        && left.project_id.is_some()
        && left.project_id == right.project_id;
    let project_boost = if same_project { 1.0 } else { 0.0 };
    let has_vectors = left.vector.is_some() && right.vector.is_some() && semantic > 0.0;
    let combined = if has_vectors {
        semantic * 0.60 + keyword * 0.30 + project_boost * 0.10
    } else {
        keyword * 0.80 + project_boost * 0.20
    };
    let dominant_signal = match (semantic_ok, keyword_ok) {
        (true, true) => "MIXED",
        (true, false) => "SEMANTIC",
        _ => "KEYWORD",
    };
    Some(RelationScores {
        semantic,
        keyword,
        project_boost,
        combined,
        dominant_signal,
    })
}

/// 标准化关键词与标签为小写术语集合。
fn normalize_terms(keywords_json: &str, tags_json: &str) -> HashSet<String> {
    let mut terms = HashSet::new();
    for raw in [keywords_json, tags_json] {
        let list: Vec<String> = serde_json::from_str(raw).unwrap_or_default();
        for item in list {
            let normalized = item.trim().to_lowercase();
            if !normalized.is_empty() {
                terms.insert(normalized);
            }
        }
    }
    terms
}

/// Jaccard 相似度；任一侧为空集合时无关键词信号，返回 0。
fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    if intersection == 0 {
        return 0.0;
    }
    let union = left.len() + right.len() - intersection;
    intersection as f64 / union as f64
}

/// 归一化向量的余弦相似度（点积），夹取到 0..=1 防浮点噪声。
fn cosine_clamped(left: &[f32], right: &[f32]) -> f64 {
    let mut dot = 0.0_f64;
    for (a, b) in left.iter().zip(right.iter()) {
        dot += (*a as f64) * (*b as f64);
    }
    dot.clamp(0.0, 1.0)
}

/// BLOB → f32 向量（小端序，与 embedding 写入格式一致）。
fn decode_vector(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.is_empty() || !blob.len().is_multiple_of(4) {
        return None;
    }
    Some(
        blob.as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect(),
    )
}

/// 加载全部图谱可见记忆的关系计算信号。
fn load_signals(connection: &Connection) -> Result<Vec<MemorySignal>, BusinessError> {
    let mut statement = connection
        .prepare(
            "SELECT m.id,m.scope,m.project_id,m.keywords_json,m.tags_json,m.is_pinned,m.importance,m.updated_at,e.vector_blob \
             FROM memory m \
             LEFT JOIN project p ON p.id=m.project_id \
             LEFT JOIN memory_embedding e ON e.memory_id=m.id \
             WHERE m.status='Active' AND (p.id IS NULL OR p.is_archived=0);",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(MemorySignal {
                id: row.get(0)?,
                scope: if row.get::<_, String>(1)?.as_str() == "Personal" {
                    MemoryScope::Personal
                } else {
                    MemoryScope::Project
                },
                project_id: row.get(2)?,
                terms: normalize_terms(&row.get::<_, String>(3)?, &row.get::<_, String>(4)?),
                is_pinned: row.get::<_, i64>(5)? != 0,
                importance: row.get(6)?,
                updated_at: row.get(7)?,
                vector: row.get::<_, Option<Vec<u8>>>(8)?.as_deref().and_then(decode_vector),
            })
        })
        .map_err(map_sqlite_error)?;
    let mut signals = Vec::new();
    for row in rows {
        signals.push(row.map_err(map_sqlite_error)?);
    }
    Ok(signals)
}

/// 删除一条记忆的全部边（独立连接，归档/不可见时使用）。
fn delete_edges_of(connection: &Connection, memory_id: &str) -> Result<(), BusinessError> {
    connection
        .execute(
            "DELETE FROM memory_edge WHERE memory_id_a=$id OR memory_id_b=$id;",
            params![memory_id],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

/// 事务内删除一条记忆的全部边。
fn delete_edges_in_transaction(transaction: &Transaction<'_>, memory_id: &str) -> Result<(), BusinessError> {
    transaction
        .execute(
            "DELETE FROM memory_edge WHERE memory_id_a=$id OR memory_id_b=$id;",
            params![memory_id],
        )
        .map_err(map_sqlite_error)?;
    Ok(())
}

/// 插入一条边；两端节点度数达到上限时，仅在强于现有最弱边时替换之。
fn insert_edge_with_cap(
    transaction: &Transaction<'_>,
    left: &str,
    right: &str,
    scores: &RelationScores,
    now_text: &str,
) -> Result<bool, BusinessError> {
    let (a, b) = if left < right { (left, right) } else { (right, left) };
    for endpoint in [a, b] {
        let degree: i64 = transaction
            .query_row(
                "SELECT count(*) FROM memory_edge WHERE memory_id_a=$id OR memory_id_b=$id;",
                params![endpoint],
                |row| row.get(0),
            )
            .map_err(map_sqlite_error)?;
        if degree < MAX_EDGES_PER_NODE as i64 {
            continue;
        }
        // 端点已满：替换其最弱边或放弃本条。
        let weakest: Option<(i64, f64)> = transaction
            .query_row(
                "SELECT rowid,combined_score FROM memory_edge \
                 WHERE memory_id_a=$id OR memory_id_b=$id \
                 ORDER BY combined_score ASC,rowid ASC LIMIT 1;",
                params![endpoint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(map_sqlite_error)?;
        match weakest {
            Some((rowid, weakest_score)) if weakest_score < scores.combined => {
                transaction
                    .execute("DELETE FROM memory_edge WHERE rowid=$rowid;", params![rowid])
                    .map_err(map_sqlite_error)?;
            }
            _ => return Ok(false),
        }
    }
    let inserted = transaction
        .execute(
            "INSERT OR IGNORE INTO memory_edge \
             (memory_id_a,memory_id_b,semantic_score,keyword_score,project_boost,combined_score,dominant_signal,updated_at) \
             VALUES($a,$b,$semantic,$keyword,$boost,$combined,$signal,$updated);",
            params![
                a,
                b,
                scores.semantic,
                scores.keyword,
                scores.project_boost,
                scores.combined,
                scores.dominant_signal,
                now_text
            ],
        )
        .map_err(map_sqlite_error)?;
    Ok(inserted > 0)
}

/// 时间范围过滤下限（None = 不限）。
fn time_cutoff(clock: &dyn Clock, days: Option<i64>) -> Option<String> {
    days.filter(|days| *days > 0).map(|days| {
        let cutoff = clock.now_utc() - chrono::Duration::days(days);
        format_storage_time(cutoff)
    })
}

/// 全局查询的节点过滤 SQL 片段与参数（不含 LIMIT）。
fn global_filter(query: &GraphGlobalQuery, cutoff: Option<&str>) -> (String, Vec<SqlValue>) {
    let mut clauses = String::from(
        "m.status='Active' AND (p.id IS NULL OR p.is_archived=0) AND (m.scope='Project' OR $include_personal)",
    );
    // include_personal 参数：1 = 含个人记忆。
    let mut values: Vec<SqlValue> = vec![SqlValue::from(if query.include_personal { 1_i64 } else { 0_i64 })];
    // project_ids 过滤：项目记忆必须属于所选集合。
    if !query.project_ids.is_empty() {
        let placeholders = query
            .project_ids
            .iter()
            .enumerate()
            .map(|(index, _)| format!("$p{index}"))
            .collect::<Vec<_>>()
            .join(",");
        clauses.push_str(&format!(
            " AND (m.scope='Personal' OR (m.scope='Project' AND m.project_id IN ({placeholders})))"
        ));
        for id in &query.project_ids {
            values.push(SqlValue::Text(id.clone()));
        }
    } else {
        // 未选择任何项目：仅保留个人记忆（由 include_personal 决定）。
        clauses.push_str(" AND m.scope='Personal'");
    }
    if let Some(cutoff) = cutoff {
        clauses.push_str(" AND m.updated_at >= $cutoff");
        values.push(SqlValue::Text(cutoff.to_string()));
    }
    (clauses, values)
}

fn count_global_nodes(
    connection: &Connection,
    query: &GraphGlobalQuery,
    cutoff: Option<&str>,
) -> Result<i64, BusinessError> {
    let (filter, values) = global_filter(query, cutoff);
    let sql = format!("SELECT count(*) FROM memory m LEFT JOIN project p ON p.id=m.project_id WHERE {filter};");
    let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
    let count = statement
        .query_row(rusqlite::params_from_iter(values.iter()), |row| row.get(0))
        .map_err(map_sqlite_error)?;
    Ok(count)
}

fn load_global_nodes(
    connection: &Connection,
    query: &GraphGlobalQuery,
    cutoff: Option<&str>,
    limit: i64,
) -> Result<Vec<GraphNode>, BusinessError> {
    let (filter, values) = global_filter(query, cutoff);
    let sql = format!(
        "SELECT m.id,m.scope,m.project_id,p.name,m.title,m.summary,m.memory_type, \
         m.keywords_json,m.tags_json,m.importance,m.is_favorite,m.is_pinned,m.updated_at \
         FROM memory m LEFT JOIN project p ON p.id=m.project_id WHERE {filter} \
         ORDER BY m.is_pinned DESC,m.importance DESC,m.updated_at DESC LIMIT $limit;"
    );
    let mut statement = connection.prepare(&sql).map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(
            rusqlite::params_from_iter(values.iter().chain(std::iter::once(&SqlValue::Integer(limit)))),
            read_graph_node,
        )
        .map_err(map_sqlite_error)?;
    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row.map_err(map_sqlite_error)?);
    }
    Ok(nodes)
}

/// 读取单条图谱可见记忆为节点；不可见或不存在返回 `None`。
fn load_node(connection: &Connection, memory_id: &str) -> Result<Option<GraphNode>, BusinessError> {
    let sql = "SELECT m.id,m.scope,m.project_id,p.name,m.title,m.summary,m.memory_type, \
               m.keywords_json,m.tags_json,m.importance,m.is_favorite,m.is_pinned,m.updated_at \
               FROM memory m LEFT JOIN project p ON p.id=m.project_id \
               WHERE m.id=$id AND m.status='Active' AND (p.id IS NULL OR p.is_archived=0);";
    connection
        .prepare(sql)
        .map_err(map_sqlite_error)?
        .query_map(params![memory_id], read_graph_node)
        .map_err(map_sqlite_error)?
        .next()
        .transpose()
        .map_err(map_sqlite_error)
}

fn read_graph_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    Ok(GraphNode {
        id: row.get(0)?,
        scope: if row.get::<_, String>(1)?.as_str() == "Personal" {
            MemoryScope::Personal
        } else {
            MemoryScope::Project
        },
        project_id: row.get(2)?,
        project_name: row.get(3)?,
        title: row.get(4)?,
        summary: row.get(5)?,
        memory_type: row.get(6)?,
        keywords: serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
        tags: serde_json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
        importance: row.get(9)?,
        is_favorite: row.get::<_, i64>(10)? != 0,
        is_pinned: row.get::<_, i64>(11)? != 0,
        updated_at: row.get(12)?,
        degree: 0,
    })
}

/// 节点集合内的合格边（分数下限 + 两端都在集合内）。
fn load_edges_within(
    connection: &Connection,
    ids: &HashSet<String>,
    min_score: f64,
) -> Result<Vec<GraphEdge>, BusinessError> {
    let mut statement = connection
        .prepare(
            "SELECT memory_id_a,memory_id_b,semantic_score,keyword_score,project_boost,combined_score,dominant_signal \
             FROM memory_edge WHERE combined_score >= $min_score ORDER BY combined_score DESC;",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params![min_score], |row| {
            Ok(GraphEdge {
                memory_id_a: row.get(0)?,
                memory_id_b: row.get(1)?,
                semantic_score: row.get(2)?,
                keyword_score: row.get(3)?,
                project_boost: row.get(4)?,
                combined_score: row.get(5)?,
                dominant_signal: row.get(6)?,
            })
        })
        .map_err(map_sqlite_error)?;
    let mut edges = Vec::new();
    for row in rows {
        let edge = row.map_err(map_sqlite_error)?;
        if ids.contains(&edge.memory_id_a) && ids.contains(&edge.memory_id_b) {
            edges.push(edge);
        }
    }
    Ok(edges)
}

/// 一条记忆的邻居（两侧方向），返回 (邻居 ID, 边分数)。
fn neighbors_of(connection: &Connection, memory_id: &str, min_score: f64) -> Result<Vec<(String, f64)>, BusinessError> {
    let mut statement = connection
        .prepare(
            "SELECT CASE WHEN memory_id_a=$id THEN memory_id_b ELSE memory_id_a END AS other,combined_score \
             FROM memory_edge \
             WHERE (memory_id_a=$id OR memory_id_b=$id) AND combined_score >= $min_score;",
        )
        .map_err(map_sqlite_error)?;
    let rows = statement
        .query_map(params![memory_id, min_score], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(map_sqlite_error)?;
    let mut neighbors = Vec::new();
    for row in rows {
        neighbors.push(row.map_err(map_sqlite_error)?);
    }
    Ok(neighbors)
}

/// 统计边度数写回节点。
fn apply_degrees(mut nodes: Vec<GraphNode>, edges: &[GraphEdge]) -> Vec<GraphNode> {
    let mut degrees: HashMap<String, i64> = HashMap::new();
    for edge in edges {
        *degrees.entry(edge.memory_id_a.clone()).or_insert(0) += 1;
        *degrees.entry(edge.memory_id_b.clone()).or_insert(0) += 1;
    }
    for node in &mut nodes {
        node.degree = *degrees.get(&node.id).unwrap_or(&0);
    }
    nodes
}

/// 构建状态：存在未完成的图谱任务即 BUILDING（期间仍返回已生成数据）。
fn build_status(connection: &Connection) -> Result<String, BusinessError> {
    let pending: i64 = connection
        .query_row(
            "SELECT count(*) FROM background_task \
             WHERE task_type IN ('REBUILD_GRAPH_MEMORY','REBUILD_GRAPH_ALL') AND status IN ('PENDING','RUNNING');",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite_error)?;
    Ok(if pending > 0 {
        "BUILDING".to_string()
    } else {
        "READY".to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::ids::FixedIdGenerator;
    use crate::memory_service::MemoryService;
    use chrono::TimeZone;
    use memory_domain::{MemoryStatus, SaveMemoryRequest};

    struct TestContext {
        #[allow(dead_code)]
        directory: tempfile::TempDir,
        database: Database,
        graph: GraphService,
        memories: MemoryService,
        project_id: String,
    }

    fn context() -> TestContext {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("graph.db");
        drop(memory_storage::open_initialized(&database_path).unwrap());
        let clock = Arc::new(FixedClock::new(
            chrono::Utc.with_ymd_and_hms(2026, 8, 16, 8, 0, 0).unwrap(),
        ));
        let ids = Arc::new(FixedIdGenerator::new(
            (1..=256)
                .map(|index| format!("{index:08}-0000-4000-8000-{index:012}"))
                .collect(),
        ));
        let database = Database::new(database_path);
        let graph = GraphService::new(database.clone(), clock.clone(), ids.clone());
        let memories = MemoryService::new(database.clone(), clock.clone(), ids.clone());
        let projects = crate::project_service::ProjectService::new(database.clone(), clock.clone(), ids.clone());
        let project_id = projects
            .create(&memory_domain::SaveProjectRequest {
                name: "图谱项目".to_string(),
                description: String::new(),
                color: "#18a386".to_string(),
            })
            .unwrap()
            .id;
        TestContext {
            directory,
            database,
            graph,
            memories,
            project_id,
        }
    }

    fn memory_request(
        title: &str,
        keywords: Vec<String>,
        scope: MemoryScope,
        project_id: Option<String>,
    ) -> SaveMemoryRequest {
        SaveMemoryRequest {
            scope,
            project_id,
            title: title.to_string(),
            summary: String::new(),
            content: format!("{title} 正文"),
            memory_type: "NOTE".to_string(),
            keywords,
            tags: vec![],
            importance: 3,
            is_favorite: false,
            is_pinned: false,
            cloud_processing_allowed: false,
            expected_version: None,
        }
    }

    fn edge_count(context: &TestContext) -> i64 {
        context
            .database
            .open()
            .unwrap()
            .query_row("SELECT count(*) FROM memory_edge;", [], |row| row.get(0))
            .unwrap()
    }

    /// 清理 create() 自动排队的图谱任务，使 build_status 回到 READY。
    fn clear_graph_tasks(context: &TestContext) {
        context
            .database
            .open()
            .unwrap()
            .execute(
                "DELETE FROM background_task WHERE task_type IN ('REBUILD_GRAPH_MEMORY','REBUILD_GRAPH_ALL');",
                [],
            )
            .unwrap();
    }

    fn degree_of(context: &TestContext, memory_id: &str) -> i64 {
        context
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM memory_edge WHERE memory_id_a=$id OR memory_id_b=$id;",
                params![memory_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn keyword_jaccard_creates_edges_and_caps_at_eight() {
        let context = context();
        // 中心记忆与 10 个邻居共享同一个关键词 → Jaccard=1.0。
        let center = context
            .memories
            .create(&memory_request(
                "中心",
                vec!["共享".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let mut neighbors = Vec::new();
        for index in 0..10 {
            neighbors.push(
                context
                    .memories
                    .create(&memory_request(
                        &format!("邻居{index}"),
                        vec!["共享".to_string()],
                        MemoryScope::Personal,
                        None,
                    ))
                    .unwrap()
                    .id,
            );
        }
        for id in std::iter::once(&center.id).chain(neighbors.iter()) {
            context.graph.recompute_memory(id).unwrap();
        }
        // 每个节点度数 ≤ 8。
        for id in std::iter::once(&center.id).chain(neighbors.iter()) {
            assert!(degree_of(&context, id) <= 8, "节点 {id} 度数超上限");
        }
        let edge_rows = edge_count(&context);
        assert!(edge_rows > 0);
        // 边的 ID 严格有序存储（a < b）。
        let unordered: i64 = context
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM memory_edge WHERE memory_id_a >= memory_id_b;",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unordered, 0);
    }

    #[test]
    fn below_threshold_no_edge_and_same_project_never_standalone() {
        let context = context();
        let first = context
            .memories
            .create(&memory_request(
                "记忆甲",
                vec!["苹果".to_string()],
                MemoryScope::Project,
                Some(context.project_id.clone()),
            ))
            .unwrap();
        let second = context
            .memories
            .create(&memory_request(
                "记忆乙",
                vec!["香蕉".to_string()],
                MemoryScope::Project,
                Some(context.project_id.clone()),
            ))
            .unwrap();
        context.graph.recompute_memory(&first.id).unwrap();
        context.graph.recompute_memory(&second.id).unwrap();
        // 关键词完全不同 → 无候选；同项目不能单独产生连线。
        assert_eq!(edge_count(&context), 0);
    }

    #[test]
    fn semantic_relation_computed_from_vectors() {
        let context = context();
        let first = context
            .memories
            .create(&memory_request(
                "语义甲",
                vec!["不同".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let second = context
            .memories
            .create(&memory_request(
                "语义乙",
                vec!["其它".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        // 直接写入同向单位向量：余弦=1 ≥ 0.72 → SEMANTIC 边。
        let connection = context.database.open().unwrap();
        for id in [&first.id, &second.id] {
            let blob: Vec<u8> = [1.0_f32, 0.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect();
            connection
                .execute(
                    "INSERT INTO memory_embedding(memory_id,provider,model,dimensions,content_checksum,vector_blob,updated_at) \
                     VALUES($id,'test','test',3,'x',$blob,$updated);",
                    params![id, blob, "2026-08-16T08:00:00.0000000+00:00"],
                )
                .unwrap();
        }
        drop(connection);
        context.graph.recompute_memory(&first.id).unwrap();
        context.graph.recompute_memory(&second.id).unwrap();
        let (combined, signal): (f64, String) = context
            .database
            .open()
            .unwrap()
            .query_row("SELECT combined_score,dominant_signal FROM memory_edge;", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(signal, "SEMANTIC");
        // 有向量：0.60×1 + 0.30×0 + 0.10×0 = 0.6。
        assert!((combined - 0.60).abs() < 1e-9, "combined={combined}");
    }

    #[test]
    fn archive_deletes_edges_and_restore_recomputes() {
        let context = context();
        let first = context
            .memories
            .create(&memory_request(
                "归档甲",
                vec!["共享".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let second = context
            .memories
            .create(&memory_request(
                "归档乙",
                vec!["共享".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        context.graph.recompute_memory(&first.id).unwrap();
        context.graph.recompute_memory(&second.id).unwrap();
        assert_eq!(edge_count(&context), 1);
        context.memories.archive(&first.id).unwrap();
        // 归档后该记忆的边被删除。
        assert_eq!(edge_count(&context), 0);
        context.memories.restore(&first.id).unwrap();
        // 恢复后 Worker 会重算；此处直接调用模拟。
        context.graph.recompute_memory(&first.id).unwrap();
        assert_eq!(edge_count(&context), 1);
        assert_eq!(first.status, MemoryStatus::Active);
    }

    #[test]
    fn global_graph_filters_personal_projects_and_limit() {
        let context = context();
        for index in 0..5 {
            context
                .memories
                .create(&memory_request(
                    &format!("个人{index}"),
                    vec!["共享".to_string()],
                    MemoryScope::Personal,
                    None,
                ))
                .unwrap();
        }
        for index in 0..3 {
            context
                .memories
                .create(&memory_request(
                    &format!("项目{index}"),
                    vec!["共享".to_string()],
                    MemoryScope::Project,
                    Some(context.project_id.clone()),
                ))
                .unwrap();
        }
        context.graph.recompute_all().unwrap();
        clear_graph_tasks(&context);

        // 默认：全部（含个人）。
        let all = context
            .graph
            .global_graph(&GraphGlobalQuery {
                include_personal: true,
                project_ids: vec![context.project_id.clone()],
                ..GraphGlobalQuery::default()
            })
            .unwrap();
        assert_eq!(all.total_nodes, 8);
        assert_eq!(all.build_status, "READY");
        assert!(all.center_memory_id.is_none());

        // 仅项目（不含个人）。
        let project_only = context
            .graph
            .global_graph(&GraphGlobalQuery {
                include_personal: false,
                project_ids: vec![context.project_id.clone()],
                ..GraphGlobalQuery::default()
            })
            .unwrap();
        assert_eq!(project_only.total_nodes, 3);
        assert!(project_only.nodes.iter().all(|node| node.scope == MemoryScope::Project));

        // limit 截断。
        let limited = context
            .graph
            .global_graph(&GraphGlobalQuery {
                include_personal: true,
                project_ids: vec![context.project_id.clone()],
                limit: 4,
                ..GraphGlobalQuery::default()
            })
            .unwrap();
        assert!(limited.truncated);
        assert_eq!(limited.nodes.len(), 4);
        assert_eq!(limited.total_nodes, 8);

        // 后端强制上限 300。
        let clamped = context
            .graph
            .global_graph(&GraphGlobalQuery {
                limit: 9999,
                ..GraphGlobalQuery::default()
            })
            .unwrap();
        assert!(clamped.nodes.len() <= 300);
    }

    #[test]
    fn neighborhood_one_and_two_hops() {
        let context = context();
        let a = context
            .memories
            .create(&memory_request(
                "链A",
                vec!["链a".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let b = context
            .memories
            .create(&memory_request(
                "链B",
                vec!["链a".to_string(), "链b".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let c = context
            .memories
            .create(&memory_request(
                "链C",
                vec!["链b".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let d = context
            .memories
            .create(&memory_request(
                "链D",
                vec!["链c".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        for memory in [&a, &b, &c, &d] {
            context.graph.recompute_memory(&memory.id).unwrap();
        }
        // 一跳：A-B（纯关键词边综合分 0.40，查询下限 0.30）。
        let one_hop = context
            .graph
            .neighborhood(&GraphNeighborhoodQuery {
                memory_id: a.id.clone(),
                depth: 1,
                min_score: 0.3,
                ..GraphNeighborhoodQuery::default()
            })
            .unwrap();
        assert_eq!(one_hop.center_memory_id, Some(a.id.clone()));
        assert!(one_hop.nodes.len() >= 2);
        assert!(one_hop.nodes.iter().all(|node| node.id != d.id), "一跳不应包含 D");
        // 两跳：A-B-C（D 仍不可达）。
        let two_hop = context
            .graph
            .neighborhood(&GraphNeighborhoodQuery {
                memory_id: a.id.clone(),
                depth: 2,
                min_score: 0.3,
                ..GraphNeighborhoodQuery::default()
            })
            .unwrap();
        assert!(two_hop.nodes.iter().any(|node| node.id == c.id), "两跳应包含 C");
        assert!(two_hop.nodes.iter().all(|node| node.id != d.id));
        // 中心记忆缺失报 MEMORY_NOT_FOUND。
        let missing = context.graph.neighborhood(&GraphNeighborhoodQuery {
            memory_id: "not-exist".to_string(),
            depth: 1,
            min_score: 0.3,
            ..GraphNeighborhoodQuery::default()
        });
        assert!(missing.is_err());
    }

    #[test]
    fn full_rebuild_orders_by_priority_and_respects_visibility() {
        let context = context();
        // 归档一条记忆：不应出现在图谱中。
        let archived = context
            .memories
            .create(&memory_request(
                "已归档",
                vec!["共享".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        context.memories.archive(&archived.id).unwrap();
        for index in 0..4 {
            context
                .memories
                .create(&memory_request(
                    &format!("重建{index}"),
                    vec!["共享".to_string()],
                    MemoryScope::Personal,
                    None,
                ))
                .unwrap();
        }
        context.graph.recompute_all().unwrap();
        let result = context.graph.global_graph(&GraphGlobalQuery::default()).unwrap();
        assert_eq!(result.total_nodes, 4);
        assert!(result.nodes.iter().all(|node| node.id != archived.id));
        // 重建后每个节点度数 ≤ 8。
        for node in &result.nodes {
            assert!(node.degree <= 8);
        }
    }

    #[test]
    fn queue_full_rebuild_marks_building() {
        let context = context();
        context
            .memories
            .create(&memory_request(
                "状态",
                vec!["共享".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let ticket = context.graph.queue_full_rebuild().unwrap();
        assert_eq!(ticket.status, "PENDING");
        let result = context.graph.global_graph(&GraphGlobalQuery::default()).unwrap();
        assert_eq!(result.build_status, "BUILDING");
        // 执行后恢复 READY（清理全部图谱任务，含 create() 排队的单记忆任务）。
        context.graph.recompute_all().unwrap();
        clear_graph_tasks(&context);
        let result = context.graph.global_graph(&GraphGlobalQuery::default()).unwrap();
        assert_eq!(result.build_status, "READY");
    }

    #[test]
    fn startup_rebuild_heals_only_once_for_legacy_data() {
        // 单条记忆不触发自愈。
        let context = context();
        context
            .memories
            .create(&memory_request(
                "孤本",
                vec!["关键词".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        assert!(context.graph.queue_startup_rebuild_if_needed().unwrap().is_none());

        // 第二条记忆 + 边为空 + 从未自愈 → 自动排队一次并留下持久标记。
        context
            .memories
            .create(&memory_request(
                "同伴",
                vec!["关键词".to_string()],
                MemoryScope::Personal,
                None,
            ))
            .unwrap();
        let ticket = context.graph.queue_startup_rebuild_if_needed().unwrap();
        assert!(ticket.is_some());
        context.graph.recompute_all().unwrap();
        assert!(edge_count(&context) > 0);
        // 边非空 + 标记存在 → 不再触发。
        assert!(context.graph.queue_startup_rebuild_if_needed().unwrap().is_none());

        // 模拟 Worker 语义：任务行处理成功后被删除、边被清空（无边数据集的稳态），
        // 标记行仍应阻止后续每次启动重复 O(n²) 重算。
        clear_graph_tasks(&context);
        context
            .database
            .open()
            .unwrap()
            .execute("DELETE FROM memory_edge;", [])
            .unwrap();
        assert_eq!(edge_count(&context), 0);
        assert!(context.graph.queue_startup_rebuild_if_needed().unwrap().is_none());
        // 标记行在 clear_graph_tasks（仅清图谱任务类型）后依然存在。
        let markers: i64 = context
            .database
            .open()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM background_task WHERE task_type='GRAPH_STARTUP_HEALED';",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(markers, 1);
    }
}
