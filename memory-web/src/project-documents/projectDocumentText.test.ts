import { describe, expect, it } from 'vitest'

import { stripYamlHeader } from './projectDocumentText'

describe('stripYamlHeader', () => {
  it('移除文件首部的 YAML 头', () => {
    expect(stripYamlHeader('---\ndocumentType: CONTEXT\n---\n# 项目背景')).toBe('# 项目背景')
  })

  it('兼容 Windows 换行符', () => {
    expect(stripYamlHeader('---\r\ndocumentType: CONTEXT\r\n---\r\n# 项目背景')).toBe('# 项目背景')
  })

  it('正文没有完整 YAML 头时保持原样', () => {
    expect(stripYamlHeader('# 项目背景\n\n正文')).toBe('# 项目背景\n\n正文')
    expect(stripYamlHeader('---\ndocumentType: CONTEXT')).toBe('---\ndocumentType: CONTEXT')
  })
})
