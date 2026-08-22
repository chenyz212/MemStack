# MemStack 知识图谱 V2 设计文档

## 一、背景与问题

### 1.1 当前状态

当前 MemStack 图谱实现了以下能力：

| 维度 | 现状 |
|---|---|
| **后端** | Rust `GraphService`，支持 `global_graph` / `neighborhood` 查询 |
| **上限** | 硬编码 `MAX_GRAPH_LIMIT = 300` |
| **布局** | 圆形约束 ForceAtlas（黄金角初始分布 + 软/硬边界） |
| **渲染** | Canvas 2D，贝塞尔曲线连线 + 流动粒子 + 多层光晕 |
| **主题** | 深色星云风格为主 |
| **交互** | 拖拽/缩放/平移/悬停聚焦/搜索定位 |
| **标签** | 默认显示，可通过"节点标签"开关控制 |

### 1.2 核心问题

1. **固定上限 300 节点**：当用户有 5000+ 条记忆时，图谱只能展示前 300 条（按重要度/置顶排序），大量记忆不可见
2. **圆形约束布局**：硬边界导致节点密集时过度挤压，松散时分布不均
3. **前端物理模拟**：每帧 O(N²) 斥力计算，500+ 节点时开始卡顿
4. **搜索弱**：搜索只是在已有节点中定位，不能扩展语义上下文
5. **无搜索驱动视图**：用户无法通过搜索快速聚焦到特定主题的记忆网络

### 1.3 用户需求

> "如果有 1 万条记忆呢？500 个节点封顶，用户不可能通过点击找记忆，肯定要搜索。搜索时把图谱变成搜索结果 + 语义相关节点。"

## 二、设计目标

### 2.1 核心定位

**图谱 = 搜索的可视化表达**，而非独立的导航工具。

| 对比 | Obsidian | MemStack V2 |
|---|---|---|
| 图谱角色 | 独立导航视图 | 搜索结果可视化辅助 |
| 大规模处理 | 限制/卡顿 | 固定上限 + 搜索切换 |
| 用户路径 | 手动浏览图谱 | 搜索 → 图谱展示上下文 |
| 节点数 | 不限制（会卡） | 固定 ≤ 500（保证流畅） |

### 2.2 设计原则

1. **500 节点硬上限**：任何视图模式最多渲染 500 个节点
2. **搜索驱动图谱**：搜索时图谱实时切换为"搜索结果 + 语义邻居"
3. **搜索是主路径**：图谱是搜索的补充，不是独立的浏览工具
4. **60 FPS 流畅**：500 节点下保持稳定帧率
5. **浅色优先**：默认浅色主题，同时支持深色

## 三、视图模式设计

### 3.1 四种视图模式

| 模式 | 节点来源 | 最大节点数 | 触发方式 |
|---|---|---|---|
| **默认视图** | Top 500（度数 + 重要度 + 最近访问 + 收藏） | 500 | 打开图谱页时 |
| **搜索视图** | 搜索结果 + 1-2 跳语义邻居（Top 500） | 500 | 搜索框输入时 |
| **项目视图** | 指定项目的记忆 + 跨项目连接（Top 500） | 500 | 点击项目筛选 |
| **局部视图** | 选中节点 + 1-2 跳邻居（Top 100） | 100 | 双击节点 / 从记忆详情进入 |

### 3.2 默认视图：核心记忆网络

用户打开图谱页时，展示最重要的 500 条记忆：

```
排序算法：
1. 收藏（+200 分）
2. 置顶（+100 分）
3. 连接数（度数 Top N）
4. 重要度（importance × 10）
5. 最近访问时间（衰减权重）
6. 最近更新时间
```

**设计意图**：展示用户最常使用、最重要的记忆，形成"核心记忆网络"。

### 3.3 搜索视图：搜索即图谱

用户在搜索框输入时，图谱实时切换：

```
用户输入: "向量嵌入"
  ↓
后端处理：
  1. FTS5 + 向量混合检索，找到 Top 20 匹配记忆
  2. 对每条匹配记忆扩展 1 跳邻居（通过 memory_edge 表）
  3. 对邻居按语义相似度 + 关键词重叠度排序
  4. 取 Top 500（匹配记忆 + 邻居）
  5. 返回节点 + 连线 + 标记（搜索结果标记为 anchor）
前端渲染：
  1. 淡入过渡动画（300ms）
  2. 搜索结果节点高亮（金色光环 + 脉动）
  3. 连线按关系类型着色
  4. 搜索无结果时显示空状态
```

### 3.4 项目视图

```
用户点击项目筛选
  ↓
后端处理：
  1. 查询该项目下所有记忆
  2. 查询跨项目的连接（通过 memory_edge）
  3. 按度数 + 重要度排序，取 Top 500
  4. 返回节点 + 连线
前端渲染：
  1. 同项目节点自然聚拢
  2. 跨项目连线用虚线/不同颜色
```

### 3.5 局部视图

```
用户双击节点 / 从记忆详情点击"查看图谱"
  ↓
后端处理：
  1. 以该记忆为中心，BFS 扩展 1-2 跳
  2. 按层级 + 边分数排序，取 Top 100
  3. 中心记忆固定在画布中心
前端渲染：
  1. 中心节点突出显示（更大 + 发光 + 脉动）
  2. 邻居按距离分层布局
  3. 点击邻居节点可切换中心
```

## 四、后端设计（Rust）

### 4.1 数据模型

#### 4.1.1 图谱节点（新增字段）

```rust
pub struct GraphNode {
    // 现有字段
    pub id: String,
    pub scope: MemoryScope,
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub title: String,
    pub summary: String,
    pub memory_type: String,
    pub keywords: Vec<String>,
    pub tags: Vec<String>,
    pub importance: i64,
    pub is_favorite: bool,
    pub is_pinned: bool,
    pub updated_at: String,
    pub degree: i64,
    // 新增字段
    pub view_score: f64,        // 视图排名分（用于 Top N 排序）
    pub is_anchor: bool,        // 是否为搜索命中节点
    pub semantic_score: f64,    // 与搜索词的语义相似度
}
```

#### 4.1.2 图谱查询扩展

```rust
pub enum GraphViewMode {
    Default,           // 核心记忆网络
    Search { query: String },  // 搜索视图
    Project { project_ids: Vec<String> },  // 项目视图
    Local { memory_id: String, depth: i64 },  // 局部视图
}

pub struct GraphQuery {
    pub mode: GraphViewMode,
    pub limit: i64,           // 硬上限 500
    pub min_score: f64,       // 关系分数下限
    pub include_personal: bool,
    pub days: Option<i64>,
}
```

### 4.2 接口设计

#### 4.2.1 统一图谱接口（替代现有 3 个接口）

```
POST /api/graph/query
  Body: { mode, limit, minScore, includePersonal, days, projectIds, query, memoryId, depth }
  Response: GraphResult { nodes[], edges[], anchorIds[], buildStatus, truncated, totalNodes, totalEdges }
```

#### 4.2.2 搜索触发的图谱接口（专用）

```
GET /api/graph/search?query={keyword}&limit=500&minScore=0.30
  Response: GraphResult { nodes[], edges[], anchorIds[], buildStatus, truncated, totalNodes, totalEdges }
```

### 4.3 核心算法

#### 4.3.1 默认视图排序

```sql
-- 视图排名分计算
view_score =
  CASE WHEN is_favorite THEN 200 ELSE 0 END +
  CASE WHEN is_pinned THEN 100 ELSE 0 END +
  degree * 5 +
  importance * 10 +
  CASE WHEN updated_at > now - 7d THEN 50 ELSE 0 END +
  CASE WHEN updated_at > now - 30d THEN 20 ELSE 0 END

SELECT * FROM memory
WHERE status = 'Active' AND ...
ORDER BY view_score DESC, updated_at DESC
LIMIT 500
```

#### 4.3.2 搜索视图算法

```
function search_graph(query, limit=500):
    // Step 1: 向量 + 关键词混合检索
    search_results = hybrid_search(query, top_k=20)

    // Step 2: 扩展邻居
    candidate_ids = set(search_results.map(r => r.id))
    for result in search_results:
        neighbors = graph_neighbors(result.id, depth=1, min_score=0.3)
        // 邻居按语义相似度排序
        ranked_neighbors = rank_by_semantic_similarity(query, neighbors)
        candidate_ids.update(top_20_per_neighbor)

    // Step 3: 去重 + 排序
    candidates = load_memories(candidate_ids)
    candidates.sort_by(view_score)

    // Step 4: 截断
    if len(candidates) > limit:
        candidates = candidates[:limit]
        truncated = true

    // Step 5: 查询边
    edges = load_edges_within(candidates.ids, min_score)

    return { candidates, edges, anchor_ids: search_results.ids, truncated }
```

#### 4.3.3 性能优化

| 优化项 | 说明 |
|---|---|
| **FTS5 索引** | 关键词搜索走 SQLite FTS5 全文检索 |
| **向量缓存** | Embedding 结果缓存，避免重复计算 |
| **term_index 倒排** | 关键词 → 记忆的倒排索引（已有实现） |
| **分批查询** | 邻居扩展分批查询，避免大 IN 子句 |
| **预计算视图分** | `view_score` 可在记忆变更时增量更新 |

### 4.4 常量调整

```rust
// 硬上限：任何模式最多 500 节点
pub const MAX_GRAPH_LIMIT: i64 = 500;

// 搜索视图参数
pub const SEARCH_ANCHOR_TOP_K: usize = 20;      // 搜索匹配 Top K
pub const SEARCH_NEIGHBOR_PER_ANCHOR: usize = 25; // 每个锚点扩展邻居数
pub const SEARCH_MAX_DEPTH: i64 = 2;             // 搜索邻居最大跳数
pub const SEARCH_MIN_SCORE: f64 = 0.30;          // 搜索关系最低分数

// 排序权重
pub const WEIGHT_FAVORITE: f64 = 200.0;
pub const WEIGHT_PINNED: f64 = 100.0;
pub const WEIGHT_DEGREE: f64 = 5.0;
pub const WEIGHT_IMPORTANCE: f64 = 10.0;
pub const WEIGHT_RECENT_7D: f64 = 50.0;
pub const WEIGHT_RECENT_30D: f64 = 20.0;
```

## 五、前端设计（Vue 3 + Canvas）

### 5.1 视觉设计

#### 5.1.1 主题

| 元素 | 浅色主题 | 深色主题 |
|---|---|---|
| 背景 | `#F7F7F8`（浅灰） | `#1A1A2E`（深蓝黑） |
| 画布区 | `#EFEFF2` | `#16213E` |
| 网格 | `rgba(0,0,0,0.04)` | `rgba(255,255,255,0.03)` |
| 节点默认 | 项目色实心圆 r=3~5 | 项目色实心圆 r=3~5 |
| 节点悬停 | 放大 1.3× + 发光 | 放大 1.3× + 发光 |
| 节点选中 | 外环 + 脉动 | 外环 + 脉动 |
| 搜索锚点 | 金色光环 `#F59E0B` | 金色光环 `#F59E0B` |
| 边-语义 | `#3B82F6`（蓝） | `#60A5FA` |
| 边-关键词 | `#D97706`（橙） | `#FBBF24` |
| 边-混合 | `#9333EA`（紫） | `#C084FC` |
| 文字 | `#171717` | `#E5E5E5` |

#### 5.1.2 节点结构

```
默认状态：
  ① 项目色实心圆 (r=3~5)
  ② 白色/深色底圈 (r+1)
  ③ 高光点 (r*0.3)

悬停状态：
  ① 项目色实心圆 (放大 1.3×)
  ② 发光效果 (多层径向渐变)
  ③ 显示名称 (HTML overlay, 白色气泡背景)

选中状态：
  ① 项目色实心圆 (放大 1.5×)
  ② 外环 (2px, 脉动动画)
  ③ 永久显示名称

搜索锚点：
  ① 金色光环 (outer ring, 脉动)
  ② 项目色实心圆
  ③ 标签高亮显示
```

#### 5.1.3 连线样式

```
默认：
  - 直线（非贝塞尔曲线）
  - 按 dominantSignal 着色
  - stroke-width = 0.5~1.2（按分数）
  - stroke-opacity = 0.08~0.3

聚焦（悬停/选中节点的边）：
  - 加粗 (1.8×)
  - 加亮 (opacity 0.6~0.9)
  - 发光 (shadowBlur)
  - 非聚焦边极淡 (opacity 0.02)
```

#### 5.1.4 标签

```
默认：全部隐藏
悬停：当前节点 + 直接邻居（1 跳）显示
选中：当前节点永久显示
搜索锚点：全部显示
缩放放大：重要节点（度数 ≥ 6 或置顶）自动显示
```

### 5.2 布局算法

#### 5.2.1 从圆形约束改为自由力导向

| 之前 | V2 |
|---|---|
| 硬边界 `WORLD_RADIUS = 420` | **无硬边界** |
| 软边界 0.78× 开始向心 | **仅质心回拉**（弱） |
| 黄金角圆盘初始分布 | **正态分布随机初始** |
| 斥力 620 | 斥力 400（更松散） |
| 弹簧 0.026 | 弹簧 0.035（更紧凑） |
| 重力 0.016 | 重力 0.008（更自由） |
| 单项目独立聚类 | **同项目微吸引力**（自然聚拢） |

#### 5.2.2 布局参数

```typescript
const V2_TUNING = {
  repulsion: 400,       // 斥力（更小 = 更松散）
  spring: 0.035,        // 弹簧（更大 = 更紧凑）
  linkDistance: 60,     // 期望边长
  gravity: 0.008,       // 质心回拉（更弱 = 更自由）
  damping: 0.55,        // 阻尼（更小 = 更快稳定）
  clusterPull: 0.015,   // 同项目微吸引力
}
```

#### 5.2.3 同项目聚类

```
在弹簧力之外，增加同项目节点之间的弱吸引力：
- 同项目节点：额外施加 0.015 × 距离 的吸引力
- 跨项目节点：无额外力
- 效果：同项目节点自然聚拢成簇，但不硬性约束
```

### 5.3 物理引擎优化

#### 5.3.1 空间网格加速

```
网格大小：40px × 40px
斥力计算：只计算相邻 9 格内的节点对
复杂度：O(N × average_neighbors_per_cell) ≈ O(N)
```

#### 5.3.2 拖拽优化

```
拖拽时：
- 当前节点锁定位置
- 物理引擎继续运行（邻居节点响应）
- 弹簧力实时响应拖拽位置
- 释放后自然稳定
```

#### 5.3.3 稳定检测

```
稳定条件：所有节点动能 < 0.02
稳定后：停止 rAF 循环，节省 CPU
拖拽/筛选/搜索时：重新升温
```

### 5.4 交互设计

#### 5.4.1 搜索切换动画

```
用户输入搜索词 → 300ms 防抖 → 调用 /api/graph/search
  ↓
前端接收到新数据：
  1. 旧图谱淡出（opacity → 0，200ms）
  2. 新数据加载完成
  3. 新图谱淡入（opacity 0→1，300ms）
  4. 搜索锚点金色光环高亮
  5. 力模拟运行至稳定
```

#### 5.4.2 控制面板

```
右侧面板：
├── 视图模式切换（默认/搜索/项目/局部）
├── 显示
│   ├── 节点标签（默认：悬停时）
│   ├── 关系颜色（按类型着色）
│   └── 项目聚类力（滑块 0~0.05）
├── 筛选
│   ├── 项目选择
│   ├── 个人记忆开关
│   └── 时间范围
├── 关系
│   ├── 最低强度（滑块 10%~90%）
│   ├── 连线粗细（滑块 0.5~2×）
│   └── 节点大小（滑块 0.5~2×）
└── 操作
    ├── 重置布局
    ├── 暂停/继续物理
    └── 重建关系
```

#### 5.4.3 底部工具栏

```
[+] [-] [⌗] [状态: 500/10000 节点] [重建关系]
```

### 5.5 文件结构

```
memory-web/src/graph/
├── GraphPage.vue          # 图谱主页面（V2 重写）
├── useGraphCanvas.ts      # Canvas 渲染（V2 重写）
├── layout.ts              # 布局算法（V2 重写：自由力导向）
├── physics.ts             # 物理引擎（新增：空间网格优化）
├── hitTest.ts             # 命中测试（新增：空间网格加速）
└── constants.ts           # 常量配置（新增）
```

## 六、数据库变更

### 6.1 `memory_edge` 表

现有表结构不变，新增索引：

```sql
-- 加速邻居查询
CREATE INDEX IF NOT EXISTS idx_memory_edge_a ON memory_edge(memory_id_a);
CREATE INDEX IF NOT EXISTS idx_memory_edge_b ON memory_edge(memory_id_b);
CREATE INDEX IF NOT EXISTS idx_memory_edge_score ON memory_edge(combined_score DESC);
```

### 6.2 新增 `graph_view_score` 字段

```sql
ALTER TABLE memory ADD COLUMN view_score REAL DEFAULT 0;
-- 用于加速默认视图排序
-- 在以下事件中增量更新：
-- 记忆创建/更新/删除
-- 收藏/置顶变更
-- 关系变更（度数变化）
```

## 七、API 契约

### 7.1 统一查询接口

```
POST /api/graph/query

Request:
{
  "mode": "default" | "search" | "project" | "local",
  "query": "string?",           // search 模式必填
  "memoryId": "string?",       // local 模式必填
  "depth": 1 | 2,              // local 模式，默认 2
  "limit": 500,                // 硬上限 500
  "minScore": 0.30,
  "includePersonal": true,
  "projectIds": ["uuid..."],   // project 模式
  "days": null | 7 | 30
}

Response:
{
  "nodes": [{
    "id": "string",
    "title": "string",
    "projectName": "string?",
    "scope": "Personal" | "Project",
    "importance": 5,
    "degree": 12,
    "isFavorite": false,
    "isPinned": false,
    "viewScore": 340.0,
    "isAnchor": false,          // 搜索锚点
    "semanticScore": 0.0,       // 与搜索词的语义分
    // ... 其他字段
  }],
  "edges": [{
    "memoryIdA": "string",
    "memoryIdB": "string",
    "combinedScore": 0.85,
    "dominantSignal": "SEMANTIC" | "KEYWORD" | "MIXED"
  }],
  "anchorIds": ["id1", "id2"],   // 搜索命中的节点 ID
  "buildStatus": "READY" | "BUILDING",
  "truncated": false,
  "totalNodes": 10000,
  "totalEdges": 45000
}
```

### 7.2 搜索专用接口

```
GET /api/graph/search?query={q}&limit=500&minScore=0.30

Response: 同上
```

## 八、性能目标

| 指标 | 目标 | 测试方法 |
|---|---|---|
| 500 节点初始渲染 | < 300ms | 冷启动图谱页 |
| 500 节点物理模拟稳定 | < 1s | 从加载到停止动画 |
| 搜索切换响应 | < 500ms | 输入搜索词到新图谱渲染 |
| 拖拽帧率 | ≥ 55 FPS | 持续拖拽 5 秒 |
| 缩放帧率 | ≥ 55 FPS | 连续缩放 10 次 |
| 悬停聚焦响应 | < 16ms | 鼠标悬停到边高亮 |
| 1 万条记忆搜索 | < 2s | 端到端搜索 + 图谱生成 |

## 九、实施计划

### Phase 1：后端（Rust）

| 任务 | 说明 |
|---|---|
| 1.1 | 新增 `GraphViewMode` 枚举和统一查询接口 |
| 1.2 | 实现 `search_graph` 搜索视图算法 |
| 1.3 | 实现默认视图 `view_score` 排序 |
| 1.4 | 实现项目视图和局部视图 |
| 1.5 | 数据库迁移（view_score 字段 + 索引） |
| 1.6 | 增量更新 view_score（记忆变更时） |
| 1.7 | 单元测试 + 集成测试 |

### Phase 2：前端（Vue 3 + Canvas）

| 任务 | 说明 |
|---|---|
| 2.1 | 重写布局算法：自由力导向 + 同项目聚类 |
| 2.2 | 重写物理引擎：空间网格加速 |
| 2.3 | 重写 Canvas 渲染：浅色主题 + 直线 + 类型着色 |
| 2.4 | 实现搜索切换动画 |
| 2.5 | 实现智能标签（默认隐藏，悬停显示） |
| 2.6 | 实现搜索锚点高亮 |
| 2.7 | 控制面板重写 |
| 2.8 | 性能测试 + 调优 |

### Phase 3：集成

| 任务 | 说明 |
|---|---|
| 3.1 | 前后端联调 |
| 3.2 | 真实数据测试（1000+ 记忆） |
| 3.3 | 端到端搜索 + 图谱流程测试 |
| 3.4 | 浅色/深色主题一致性 |
| 3.5 | 响应式测试（桌面/窄屏） |

## 十、与现有系统的兼容性

### 10.1 向后兼容

| 方面 | 兼容策略 |
|---|---|
| `memory_edge` 表 | 不变，仅新增索引 |
| `memory` 表 | 新增 `view_score` 字段，默认 0 |
| 现有 3 个 API | 保留，新 API 作为增强 |
| MCP 工具 | `graph_query` 工具增加 `mode` 参数 |

### 10.2 渐进迁移

- Phase 1 完成后，新接口可与旧接口并存
- Phase 2 完成后，前端切换到新接口
- 旧接口标记为 deprecated，在 V3 完全移除

## 附录：Obsidian 对比

| 维度 | Obsidian | MemStack V2 |
|---|---|---|
| 节点上限 | 无限制（实际 500+ 开始卡） | 固定 500（保证流畅） |
| 布局 | ForceAtlas 自由布局 | 自由力导向 + 同项目聚类 |
| 搜索 | 搜索结果在图谱中高亮 | 整个图谱切换为搜索结果视图 |
| 标签 | 缩放时显隐 | 默认隐藏，悬停/选中/搜索锚点显示 |
| 关系类型 | 无类型化边 | 语义/关键词/混合三色 |
| 物理引擎 | 客户端 d3-force | 客户端空间网格加速 |
| 主题 | 深色优先 | 浅色优先 |
| 数据来源 | `[[wikilinks]]` 静态链接 | `memory_edge` 语义关系（动态计算） |
