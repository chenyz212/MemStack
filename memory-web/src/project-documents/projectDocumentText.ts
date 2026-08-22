/** 去掉项目文档开头的 YAML 头，只返回正文。 */
export function stripYamlHeader(content: string): string {
  const normalized = content.replace(/\r\n/g, '\n')
  if (!normalized.startsWith('---\n')) return content

  const closingSeparator = normalized.indexOf('\n---\n', 4)
  if (closingSeparator === -1) return content
  return normalized.slice(closingSeparator + 5).trim()
}
