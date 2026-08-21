import { describe, expect, it } from 'vitest'
import { renderMemoryMarkdown } from './markdown'

describe('记忆 Markdown 渲染', () => {
  /** 验证常用 Markdown 结构会被转换为 HTML。 */
  it('解析标题列表代码和表格', () => {
    const html = renderMemoryMarkdown('# 标题\n\n- 条目\n\n```ts\nconst value = 1\n```\n\n| 名称 | 值 |\n| --- | --- |\n| A | B |')

    expect(html).toContain('<h1>标题</h1>')
    expect(html).toContain('<li>条目</li>')
    expect(html).toContain('<pre><code')
    expect(html).toContain('<table>')
  })

  /** 验证不可信记忆不能注入脚本或危险链接。 */
  it('移除脚本与危险链接', () => {
    const html = renderMemoryMarkdown('<script>alert(1)</script>\n\n[危险链接](javascript:alert(1))')

    expect(html).not.toContain('<script')
    expect(html).not.toContain('href="javascript:')
  })
})
