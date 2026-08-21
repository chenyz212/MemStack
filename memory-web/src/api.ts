import { invoke } from '@tauri-apps/api/core'

export interface ProjectItem {
  id: string
  name: string
  description: string
  color: string
  isArchived: boolean
  workspaceBound: boolean
  workspaceIdentifier: string | null
  activeMemoryCount: number
  /** 全部记忆数（含已归档），供已归档项目彻底删除确认显示 */
  totalMemoryCount: number
}

/** 彻底删除已归档项目的统计结果。 */
export interface DeletedProjectStats {
  deletedMemories: number
  deletedCandidates: number
}

export interface MemoryFacets {
  allCount: number
  personalCount: number
  projectCount: number
  favoriteCount: number
  pinnedCount: number
  archivedCount: number
  projectCounts: Record<string, number>
}

export interface McpConnectionInfo {
  endpoint: string
  protocolVersion: string
  status: string
  errorMessage: string | null
}

export type McpAssistantType = 'Codex' | 'Claude' | 'Cursor' | 'Trae' | 'Generic'
export type McpPermission = 'Read' | 'ReadWrite'

/**
 * 动态 MCP 客户端会话状态。
 * - Active：已签发并允许调用。
 * - Revoked：Token 已吊销，卡片保留可重新生成。
 * - Expired：Token 已过期，需要轮换。
 */
export type McpClientStatus = 'Active' | 'Revoked' | 'Expired'

export interface McpTokenItem {
  id: string
  assistantType: McpAssistantType
  displayName: string
  tokenPrefix: string
  permission: McpPermission
  projectId: string | null
  projectName: string | null
  expiresAt: string | null
  lastUsedAt: string | null
  createdAt: string
  revokedAt: string | null
}

export interface McpTokenSecret {
  token: McpTokenItem
  plainToken: string
}

/**
 * 动态 MCP 客户端会话的安全卡片视图，不含明文 Token。
 */
export interface McpClientCard {
  sessionId: string
  clientKey: string
  displayName: string
  clientVersion: string | null
  transport: string
  tokenPrefix: string
  permission: McpPermission
  projectId: string | null
  projectName: string | null
  expiresAt: string | null
  lastUsedAt: string | null
  callCount: number
  createdAt: string
  revokedAt: string | null
  status: McpClientStatus
}

/**
 * 创建动态 AI 工具请求体。
 */
export interface CreateMcpClientRequest {
  displayName: string
  permission: McpPermission
  projectId: string | null
  expiresAt: string | null
}

/**
 * 更新动态 AI 工具请求体。
 */
export interface UpdateMcpClientRequest {
  displayName: string
  permission: McpPermission
  projectId: string | null
  expiresAt: string | null
  clearExpiresAt: boolean
}

/**
 * 动态客户端会话的安全视图，含明文 Token（仅生成或轮换后返回）。
 */
export interface McpClientSecret {
  client: McpClientCard
  plainToken: string
}

export interface McpConnectionTestResult {
  success: boolean
  message: string
  toolCount: number
}

/**
 * AI 客户端接入配置的预览/注册结果（Rust client_registration::RegistrationReport）。
 */
export interface RegistrationReport {
  clientType: string
  supported: boolean
  written: boolean
  configPath: string | null
  backupPath: string | null
  configText: string
  message: string
}

/**
 * 客户端配置路径健康（Rust client_registration::PathHealthReport）：
 * 绿色版移动目录后检测已写入配置是否仍指向当前 MCP exe。
 */
export interface PathHealthReport {
  clientType: string
  supported: boolean
  configured: boolean
  healthy: boolean
  usesLegacyEnvToken: boolean
  registeredCommand: string | null
  configPath: string | null
}

export interface MemoryItem {
  id: string
  scope: 'Personal' | 'Project'
  projectId: string | null
  projectName: string | null
  title: string
  summary: string
  content: string
  memoryType: string
  keywords: string[]
  tags: string[]
  importance: number
  isFavorite: boolean
  isPinned: boolean
  cloudProcessingAllowed: boolean
  status: 'Active' | 'Archived'
  version: number
  createdSource: string
  updatedSource: string
  createdAt: string
  updatedAt: string
}

export interface MemoryRevisionItem {
  id: string
  memoryId: string
  version: number
  createdAt: string
}

export interface MemoryCandidateItem {
  id: string
  scope: 'Personal' | 'Project'
  projectId: string | null
  projectName: string | null
  title: string
  summary: string
  content: string
  memoryType: string
  keywords: string[]
  tags: string[]
  importance: number
  cloudProcessingAllowed: boolean
  sourceName: string
  version: number
  createdAt: string
  updatedAt: string
}

export interface SearchResult {
  memory: MemoryItem
  score: number
  matchReasons: string[]
}

export interface OverviewResult {
  memoryCount: number
  projectCount: number
  candidateCount: number
  recentMemories: MemoryItem[]
  activeProjects: ProjectItem[]
  searchMode: 'KEYWORD' | 'HYBRID'
  mcpStatus: string
  recentAssistantName: string | null
  lastMcpCallAt: string | null
  /** 最近一次 MCP 读取/创建活动（无则 null） */
  mcpActivity: McpActivitySummary | null
  /** 未吊销未过期的 AI 工具（最多 6 个，按最近使用排序） */
  recentClients: OverviewClient[]
  /** 符合条件的 AI 工具总数（供「+N」角标） */
  activeClientCount: number
}

/** MCP 最近记忆活动摘要（不含项目名、标题与正文）。 */
export interface McpActivitySummary {
  displayName: string
  action: 'READ' | 'CREATE'
  scope: 'Personal' | 'Project' | 'Mixed'
  occurredAt: string
}

/** 总览轨道上的 AI 工具。 */
export interface OverviewClient {
  displayName: string
  transport: string
  lastUsedAt: string | null
}

/** 图谱节点：一条正式记忆。 */
export interface GraphNode {
  id: string
  title: string
  summary: string
  scope: 'Personal' | 'Project'
  projectId: string | null
  projectName: string | null
  memoryType: string
  importance: number
  isFavorite: boolean
  isPinned: boolean
  tags: string[]
  keywords: string[]
  updatedAt: string
  degree: number
}

/** 图谱边：无向相似关系（memoryIdA < memoryIdB）。 */
export interface GraphEdge {
  memoryIdA: string
  memoryIdB: string
  semanticScore: number
  keywordScore: number
  projectBoost: number
  combinedScore: number
  dominantSignal: 'SEMANTIC' | 'KEYWORD' | 'MIXED'
}

/** 图谱响应。 */
export interface GraphResult {
  nodes: GraphNode[]
  edges: GraphEdge[]
  centerMemoryId: string | null
  buildStatus: 'READY' | 'BUILDING'
  truncated: boolean
  totalNodes: number
  totalEdges: number
}

/** 全局图谱查询。 */
export interface GraphGlobalQuery {
  projectIds: string[]
  includePersonal: boolean
  days: number | null
  limit: number
  minScore: number
}

/** 局部图谱查询。 */
export interface GraphNeighborhoodQuery {
  memoryId: string
  depth: number
  limit: number
  minScore: number
}

export interface CursorPage<T> {
  items: T[]
  nextCursor: string | null
  hasMore: boolean
}

export interface EmbeddingSettings {
  baseUrl: string
  model: string
  /** 明文回显（本地单机应用），界面端雾化展示 */
  apiKey: string
  dimensions: number
  enabled: boolean
  configured: boolean
}

export interface ApiErrorBody {
  code: string
  message: string
}

/** 保留后端业务代码，供界面执行针对性的恢复操作。 */
export class ApiRequestError extends Error {
  readonly code: string

  /** 创建保留业务代码的前端请求异常。 */
  constructor(code: string, message: string) {
    super(message)
    this.name = 'ApiRequestError'
    this.code = code
  }
}

/** 由路由构造函数生成：把匹配到的 HTTP 请求组装为 Tauri 命令参数。 */
type RouteBuild = (match: RegExpMatchArray, params: URLSearchParams, body: unknown) => Record<string, unknown>

/** 单条 HTTP 路由到本地命令的映射描述。 */
interface LocalRoute {
  method: string
  pattern: RegExp
  command: string
  build: RouteBuild
}

/** 声明一条 HTTP 路由到 Tauri 本地命令的映射。 */
function route(method: string, pattern: RegExp, command: string, build: RouteBuild = () => ({})): LocalRoute {
  return { method, pattern, command, build }
}

/** 把记忆列表查询参数映射为 list_memories 命令的 query 对象（只包含出现的键）。 */
function buildMemoryQuery(params: URLSearchParams): Record<string, unknown> {
  const query: Record<string, unknown> = { size: Number(params.get('size')) }
  const status = params.get('status')
  if (status !== null) query.status = status
  const scope = params.get('scope')
  if (scope !== null) query.scope = scope
  const projectId = params.get('projectId')
  if (projectId !== null) query.projectId = projectId
  if (params.has('favorite')) query.isFavorite = params.get('favorite') === 'true'
  if (params.has('pinned')) query.isPinned = params.get('pinned') === 'true'
  const memoryType = params.get('type')
  if (memoryType !== null) query.memoryType = memoryType
  const tag = params.get('tag')
  if (tag !== null) query.tag = tag
  const importanceMin = params.get('importanceMin')
  if (importanceMin !== null) query.importanceMin = Number(importanceMin)
  const cursor = params.get('cursor')
  if (cursor !== null) query.cursor = cursor
  return query
}

/** HTTP 路由到本地命令的映射表；具体路径必须排在参数路径之前。 */
const routes: LocalRoute[] = [
  // 总览 / 项目 / 工作空间
  route('GET', /^\/api\/overview$/, 'get_overview'),
  route('GET', /^\/api\/data-version$/, 'get_data_version'),
  route('GET', /^\/api\/projects$/, 'list_projects', (_match, params) => ({
    includeArchived: params.get('includeArchived') === 'true',
  })),
  route('POST', /^\/api\/projects$/, 'create_project', (_match, _params, body) => ({ request: body })),
  route('PUT', /^\/api\/projects\/([^/]+)$/, 'update_project', (match, _params, body) => ({ id: match[1], request: body })),
  route('POST', /^\/api\/projects\/([^/]+)\/archive$/, 'archive_project', (match) => ({ id: match[1] })),
  route('POST', /^\/api\/projects\/([^/]+)\/restore$/, 'restore_project', (match) => ({ id: match[1] })),
  route('DELETE', /^\/api\/projects\/([^/]+)$/, 'delete_project_permanent', (match) => ({ id: match[1] })),
  route('PUT', /^\/api\/projects\/([^/]+)\/workspace$/, 'bind_project_workspace', (match, _params, body) => ({
    projectId: match[1],
    workspaceIdentifier: (body as { workspaceIdentifier?: string } | undefined)?.workspaceIdentifier,
  })),
  route('DELETE', /^\/api\/projects\/([^/]+)\/workspace$/, 'unbind_project_workspace', (match) => ({ projectId: match[1] })),
  route('POST', /^\/api\/workspaces\/resolve$/, 'resolve_workspace', (_match, _params, body) => ({ request: body })),
  route('POST', /^\/api\/workspaces\/memories$/, 'store_workspace_memory', (_match, _params, body) => ({ request: body })),

  // 记忆（facets / quick-capture / revisions 必须先于 :id 通配）
  route('GET', /^\/api\/memories\/facets$/, 'get_memory_facets'),
  route('POST', /^\/api\/memories\/quick-capture$/, 'quick_capture_memory', (_match, _params, body) => ({ request: body })),
  route('GET', /^\/api\/memories\/([^/]+)\/revisions$/, 'list_memory_revisions', (match) => ({ id: match[1] })),
  route('POST', /^\/api\/memories\/([^/]+)\/revisions\/([^/]+)\/restore$/, 'restore_memory_revision', (match) => ({
    id: match[1],
    version: Number(match[2]),
  })),
  route('GET', /^\/api\/memories$/, 'list_memories', (_match, params) => ({ query: buildMemoryQuery(params) })),
  route('GET', /^\/api\/memories\/([^/]+)$/, 'get_memory', (match) => ({ id: match[1] })),
  route('POST', /^\/api\/memories$/, 'create_memory', (_match, _params, body) => ({ request: body })),
  route('PUT', /^\/api\/memories\/([^/]+)$/, 'update_memory', (match, _params, body) => ({ id: match[1], request: body })),
  route('POST', /^\/api\/memories\/([^/]+)\/archive$/, 'archive_memory', (match) => ({ id: match[1] })),
  route('POST', /^\/api\/memories\/([^/]+)\/restore$/, 'restore_memory', (match) => ({ id: match[1] })),
  route('DELETE', /^\/api\/memories\/([^/]+)\/permanent$/, 'delete_memory_permanently', (match) => ({ id: match[1] })),

  // 记忆候选
  route('GET', /^\/api\/memory-candidates$/, 'list_memory_candidates'),
  route('PUT', /^\/api\/memory-candidates\/([^/]+)$/, 'update_memory_candidate', (match, _params, body) => ({ id: match[1], request: body })),
  route('POST', /^\/api\/memory-candidates\/([^/]+)\/confirm$/, 'confirm_memory_candidate', (match, _params, body) => ({
    id: match[1],
    expectedVersion: (body as { expectedVersion?: number } | undefined)?.expectedVersion,
  })),
  route('POST', /^\/api\/memory-candidates\/([^/]+)\/reject$/, 'reject_memory_candidate', (match, _params, body) => ({
    id: match[1],
    expectedVersion: (body as { expectedVersion?: number } | undefined)?.expectedVersion,
  })),

  // 检索
  route('POST', /^\/api\/search$/, 'search_memories', (_match, _params, body) => ({ request: body })),
  route('POST', /^\/api\/search\/context$/, 'build_memory_context', (_match, _params, body) => ({ request: body })),

  // 记忆图谱
  route('GET', /^\/api\/graph\/global$/, 'get_global_graph', (_match, params) => ({
    request: {
      projectIds: (params.get('projectIds') ?? '').split(',').filter(Boolean),
      includePersonal: params.get('includePersonal') !== 'false',
      days: params.get('days') !== null && params.get('days') !== '' ? Number(params.get('days')) : null,
      limit: Number(params.get('limit') ?? 120),
      minScore: Number(params.get('minScore') ?? 0.45),
    },
  })),
  route('GET', /^\/api\/graph\/neighborhood$/, 'get_neighborhood_graph', (_match, params) => ({
    request: {
      memoryId: params.get('memoryId') ?? '',
      depth: Number(params.get('depth') ?? 2),
      limit: Number(params.get('limit') ?? 120),
      minScore: Number(params.get('minScore') ?? 0.45),
    },
  })),
  route('POST', /^\/api\/graph\/rebuild$/, 'rebuild_graph'),

  // Embedding 设置
  route('GET', /^\/api\/settings\/embedding$/, 'get_embedding_settings'),
  route('PUT', /^\/api\/settings\/embedding$/, 'save_embedding_settings', (_match, _params, body) => ({ request: body })),
  route('POST', /^\/api\/settings\/embedding\/rebuild$/, 'rebuild_embeddings'),

  // MCP 客户端
  route('GET', /^\/api\/mcp\/connection$/, 'get_mcp_connection'),
  route('GET', /^\/api\/mcp\/clients$/, 'list_mcp_clients'),
  route('POST', /^\/api\/mcp\/clients$/, 'create_mcp_client', (_match, _params, body) => ({ request: body })),
  route('PATCH', /^\/api\/mcp\/clients\/([^/]+)$/, 'update_mcp_client', (match, _params, body) => ({ sessionId: match[1], request: body })),
  route('POST', /^\/api\/mcp\/clients\/([^/]+)\/rotate$/, 'rotate_mcp_client_session_id', (match) => ({ sessionId: match[1] })),
  route('DELETE', /^\/api\/mcp\/clients\/([^/]+)$/, 'delete_mcp_client', (match) => ({ sessionId: match[1] })),
  route('GET', /^\/api\/mcp\/clients\/([^/]+)\/secret$/, 'reveal_mcp_client_secret', (match) => ({ sessionId: match[1] })),
  route('POST', /^\/api\/mcp\/clients\/([^/]+)\/test$/, 'test_mcp_client', (match) => ({ sessionId: match[1] })),
  route('POST', /^\/api\/mcp\/clients\/([^/]+)\/register$/, 'register_mcp_client_config', (match, _params, body) => ({
    sessionId: match[1],
    clientType: (body as { clientType?: string } | undefined)?.clientType,
  })),
  route('POST', /^\/api\/mcp\/clients\/([^/]+)\/config-preview$/, 'get_mcp_client_config_preview', (match, _params, body) => ({
    sessionId: match[1],
    clientType: (body as { clientType?: string } | undefined)?.clientType,
  })),
  route('GET', /^\/api\/mcp\/clients\/path-health$/, 'get_mcp_client_path_health', (_match, params) => ({
    clientType: params.get('clientType') ?? '',
  })),
]

/** 判断 Tauri 命令的拒绝值是否携带后端业务错误码。 */
function isCommandError(value: unknown): value is { code: string, message?: string } {
  return typeof value === 'object' && value !== null && typeof (value as { code?: unknown }).code === 'string'
}

/** 把页面里的 HTTP 风格请求转发为 Tauri 本地命令调用。 */
export async function apiFetch<T>(path: string, init: RequestInit, signal: AbortSignal | null): Promise<T> {
  void signal
  const method = (init.method ?? 'GET').toUpperCase()
  const url = new URL(path, 'http://local')
  const body = typeof init.body === 'string' && init.body.length > 0 ? JSON.parse(init.body) as unknown : undefined

  for (const entry of routes) {
    if (entry.method !== method) continue
    const match = url.pathname.match(entry.pattern)
    if (!match) continue
    try {
      return await invoke<T>(entry.command, entry.build(match, url.searchParams, body))
    } catch (error) {
      if (isCommandError(error)) {
        throw new ApiRequestError(error.code, error.message ?? '本地请求失败')
      }
      throw new ApiRequestError('REQUEST_FAILED', '本地请求失败')
    }
  }

  throw new ApiRequestError('REQUEST_FAILED', `本地请求失败：未注册的调用 ${method} ${url.pathname}`)
}
