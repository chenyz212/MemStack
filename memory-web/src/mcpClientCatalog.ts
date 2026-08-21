/**
 * MCP 客户端展示目录：把已知的 AI 工具名映射成前端展示信息。
 * 与 Java 版 memory-web/src/lib/mcpClientCatalog.ts 保持一致。
 *
 * 仅用于展示样式（颜色、图标字母、传输方式说明），不影响业务身份；
 * 未识别名称使用通用 MCP 兜底展示。
 */

export interface McpClientCatalogEntry {
  /** 匹配服务端归一化后的 clientKey（小写、移除空白字符） */
  key: string
  /** 展示名称 */
  label: string
  /** 卡片 Logo 里的首字母 */
  logoLetter: string
  /** Logo 背景色（十六进制） */
  color: string
  /** 传输方式描述 */
  transportLabel: string
  /** 卡片副标题描述 */
  description: string
}

/** 已知客户端目录 */
export const MCP_CLIENT_CATALOG: McpClientCatalogEntry[] = [
  { key: 'claudedesktop', label: 'Claude Desktop', logoLetter: 'A', color: '#bd634f', transportLabel: 'stdio', description: '默认范围：个人记忆' },
  { key: 'claude', label: 'Claude', logoLetter: 'A', color: '#bd634f', transportLabel: 'stdio', description: '默认范围：个人记忆' },
  { key: 'cursor', label: 'Cursor', logoLetter: 'C', color: '#4a65c7', transportLabel: 'HTTP', description: '默认项目：未指定时按全部命中' },
  { key: 'trae', label: 'Trae', logoLetter: 'T', color: '#5b8def', transportLabel: 'HTTP', description: 'AI 编程助手 · 共享记忆中枢' },
  { key: 'workbuddy', label: 'WorkBuddy', logoLetter: 'W', color: '#7555aa', transportLabel: 'HTTP', description: 'AI 编程助手 · 共享记忆中枢' },
  { key: 'qoderwork', label: 'QoderWork', logoLetter: 'Q', color: '#2b6cb0', transportLabel: 'HTTP', description: 'AI 编程助手 · 共享记忆中枢' },
  { key: 'qoder', label: 'Qoder', logoLetter: 'Q', color: '#2b6cb0', transportLabel: 'HTTP', description: 'AI 编程助手 · 共享记忆中枢' },
]

/** 未识别客户端的兜底展示 */
export const FALLBACK_CATALOG_ENTRY: McpClientCatalogEntry = {
  key: '*',
  label: '',
  logoLetter: '?',
  color: '#71807d',
  transportLabel: 'HTTP/stdio',
  description: '自定义 MCP 客户端',
}

/**
 * 把用户填写的 AI 工具名规范化为 clientKey。
 * 规则：去首尾空格、转小写、移除空白字符。与服务端 NormalizeClientKey 保持一致。
 */
export function normalizeClientKey(displayName: string): string {
  return displayName
    .trim()
    .toLowerCase()
    .replace(/\s+/g, '')
}

/**
 * 根据 clientKey 查找目录项，未命中时返回兜底项。
 */
export function getCatalogEntry(clientKey: string): McpClientCatalogEntry {
  const key = normalizeClientKey(clientKey)
  return MCP_CLIENT_CATALOG.find((entry) => entry.key === key) ?? FALLBACK_CATALOG_ENTRY
}

/**
 * 判断目录项是否为兜底（未识别）项。
 */
export function isFallbackEntry(entry: McpClientCatalogEntry): boolean {
  return entry.key === '*'
}

/**
 * 返回用于卡片展示的标题：优先使用目录预设名（如 codex → Codex），
 * 未识别时直接使用用户填写的原名，绝不显示"未识别客户端"死文案。
 */
export function getDisplayTitle(displayName: string): string {
  const entry = getCatalogEntry(displayName)
  return isFallbackEntry(entry) ? displayName : entry.label
}

/**
 * 返回卡片 Logo 字母：已知客户端使用目录预设字母，
 * 未识别时取用户名首字母（大写），空串兜底为"?"。
 */
export function getLogoLetter(displayName: string): string {
  const entry = getCatalogEntry(displayName)
  if (!isFallbackEntry(entry)) return entry.logoLetter
  const trimmed = displayName.trim()
  return trimmed.length > 0 ? trimmed.charAt(0).toUpperCase() : '?'
}

/**
 * 返回卡片 Logo 背景色：已知客户端使用目录预设颜色，
 * 未识别时使用兜底灰色。
 */
export function getLogoColor(displayName: string): string {
  return getCatalogEntry(displayName).color
}
