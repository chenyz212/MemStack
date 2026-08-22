import { describe, expect, it } from 'vitest'

import { formatDateTimeInTimeZone } from './projectDocumentTime'

describe('formatDateTimeInTimeZone', () => {
  it('将 UTC 时间转换为上海时区时间', () => {
    expect(formatDateTimeInTimeZone('2026-08-22T09:32:34Z', 'Asia/Shanghai')).toBe(
      '2026-08-22 17:32:34',
    )
  })

  it('显式使用 UTC 时保持 UTC 墙上时间', () => {
    expect(formatDateTimeInTimeZone('2026-08-22T09:32:34+00:00', 'UTC')).toBe(
      '2026-08-22 09:32:34',
    )
  })

  it('无法解析时保留服务端原值', () => {
    expect(formatDateTimeInTimeZone('未知时间', 'Asia/Shanghai')).toBe('未知时间')
  })
})
