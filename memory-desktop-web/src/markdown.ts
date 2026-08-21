import MarkdownIt from 'markdown-it'

const markdown = new MarkdownIt({
  html: false,
  linkify: true,
  typographer: false,
  breaks: false,
})

/** 将记忆中的 Markdown 转换为安全 HTML，禁止原始 HTML 和危险链接协议。 */
export function renderMemoryMarkdown(content: string): string {
  return markdown.render(content)
}
