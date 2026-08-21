import { beforeEach, describe, expect, it, vi } from 'vitest'
import { invoke } from '@tauri-apps/api/core'
import { ApiRequestError, apiFetch } from './api'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))

const invokeMock = vi.mocked(invoke)

/** 捕获 apiFetch 抛出的异常，便于断言错误码。 */
async function captureError(promise: Promise<unknown>): Promise<unknown> {
  return promise.then(
    () => null,
    (reason: unknown) => reason,
  )
}

/** 验证 apiFetch 将页面 HTTP 风格请求转发为 Tauri 本地命令。 */
describe('apiFetch 本地命令转发', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('GET /api/overview 调用 get_overview 并透传返回值', async () => {
    invokeMock.mockResolvedValue({ memoryCount: 3 })
    const result = await apiFetch<{ memoryCount: number }>('/api/overview', { method: 'GET' }, null)
    expect(invokeMock).toHaveBeenCalledTimes(1)
    expect(invokeMock).toHaveBeenCalledWith('get_overview', {})
    expect(result).toEqual({ memoryCount: 3 })
  })

  it('GET /api/graph/global 将查询参数映射为 get_global_graph 的 request', async () => {
    invokeMock.mockResolvedValue({ nodes: [], edges: [] })
    await apiFetch<unknown>(
      '/api/graph/global?projectIds=p1,p2&includePersonal=false&days=7&limit=120&minScore=0.3',
      { method: 'GET' },
      null,
    )
    expect(invokeMock).toHaveBeenCalledWith('get_global_graph', {
      request: {
        projectIds: ['p1', 'p2'],
        includePersonal: false,
        days: 7,
        limit: 120,
        minScore: 0.3,
      },
    })
  })

  it('GET /api/graph/global 缺省参数使用默认值（不限时间）', async () => {
    invokeMock.mockResolvedValue({ nodes: [], edges: [] })
    await apiFetch<unknown>('/api/graph/global', { method: 'GET' }, null)
    expect(invokeMock).toHaveBeenCalledWith('get_global_graph', {
      request: {
        projectIds: [],
        includePersonal: true,
        days: null,
        limit: 120,
        minScore: 0.45,
      },
    })
  })

  it('GET /api/graph/neighborhood 调用 get_neighborhood_graph', async () => {
    invokeMock.mockResolvedValue({ nodes: [], edges: [] })
    await apiFetch<unknown>(
      '/api/graph/neighborhood?memoryId=mem-1&depth=2&limit=80&minScore=0.3',
      { method: 'GET' },
      null,
    )
    expect(invokeMock).toHaveBeenCalledWith('get_neighborhood_graph', {
      request: { memoryId: 'mem-1', depth: 2, limit: 80, minScore: 0.3 },
    })
  })

  it('POST /api/graph/rebuild 调用 rebuild_graph', async () => {
    invokeMock.mockResolvedValue({ id: 'ticket', status: 'PENDING' })
    await apiFetch<unknown>('/api/graph/rebuild', { method: 'POST' }, null)
    expect(invokeMock).toHaveBeenCalledWith('rebuild_graph', {})
  })

  it('GET /api/memories 将查询参数映射为 list_memories 的 query 对象', async () => {
    invokeMock.mockResolvedValue({ items: [], nextCursor: null, hasMore: false })
    await apiFetch<unknown>(
      '/api/memories?status=Active&size=30&scope=Personal&favorite=true&type=NOTE&tag=x&importanceMin=2&cursor=abc',
      { method: 'GET' },
      null,
    )
    expect(invokeMock).toHaveBeenCalledWith('list_memories', {
      query: {
        status: 'Active',
        size: 30,
        scope: 'Personal',
        isFavorite: true,
        memoryType: 'NOTE',
        tag: 'x',
        importanceMin: 2,
        cursor: 'abc',
      },
    })
    const query = (invokeMock.mock.calls[0]?.[1] as { query: Record<string, unknown> }).query
    expect(query).not.toHaveProperty('projectId')
    expect(query).not.toHaveProperty('isPinned')
  })

  it('POST /api/memories 把解析后的 JSON 请求体作为 request 传给 create_memory', async () => {
    invokeMock.mockResolvedValue({})
    await apiFetch<unknown>('/api/memories', {
      method: 'POST',
      body: JSON.stringify({ title: '本地命令', content: '记忆内容' }),
    }, null)
    expect(invokeMock).toHaveBeenCalledWith('create_memory', {
      request: { title: '本地命令', content: '记忆内容' },
    })
  })

  it('PUT /api/projects/:id/workspace 调用 bind_project_workspace', async () => {
    invokeMock.mockResolvedValue({})
    await apiFetch<unknown>('/api/projects/p-1/workspace', {
      method: 'PUT',
      body: JSON.stringify({ workspaceIdentifier: 'ws-abc' }),
    }, null)
    expect(invokeMock).toHaveBeenCalledWith('bind_project_workspace', {
      projectId: 'p-1',
      workspaceIdentifier: 'ws-abc',
    })
  })

  it('POST /api/memory-candidates/:id/confirm 透传乐观锁版本号', async () => {
    invokeMock.mockResolvedValue({})
    await apiFetch<unknown>('/api/memory-candidates/c-9/confirm', {
      method: 'POST',
      body: JSON.stringify({ expectedVersion: 3 }),
    }, null)
    expect(invokeMock).toHaveBeenCalledWith('confirm_memory_candidate', {
      id: 'c-9',
      expectedVersion: 3,
    })
  })

  it('POST /api/mcp/clients/:id/register 调用 register_mcp_client_config', async () => {
    invokeMock.mockResolvedValue({})
    await apiFetch<unknown>('/api/mcp/clients/s-7/register', {
      method: 'POST',
      body: JSON.stringify({ clientType: 'Codex' }),
    }, null)
    expect(invokeMock).toHaveBeenCalledWith('register_mcp_client_config', {
      sessionId: 's-7',
      clientType: 'Codex',
    })
  })

  it('命令拒绝业务错误对象时抛出携带错误码的 ApiRequestError', async () => {
    invokeMock.mockRejectedValue({ code: 'CONFLICT', message: '冲突' })
    const error = await captureError(apiFetch<unknown>('/api/overview', { method: 'GET' }, null))
    expect(error).toBeInstanceOf(ApiRequestError)
    expect((error as ApiRequestError).code).toBe('CONFLICT')
    expect((error as ApiRequestError).message).toBe('冲突')
  })

  it('命令拒绝非对象值时回退为 REQUEST_FAILED', async () => {
    invokeMock.mockRejectedValue('boom')
    const error = await captureError(apiFetch<unknown>('/api/overview', { method: 'GET' }, null))
    expect(error).toBeInstanceOf(ApiRequestError)
    expect((error as ApiRequestError).code).toBe('REQUEST_FAILED')
  })

  it('未注册的调用抛出 REQUEST_FAILED 且不触发 invoke', async () => {
    const error = await captureError(apiFetch<unknown>('/api/mcp/restart', { method: 'POST' }, null))
    expect(error).toBeInstanceOf(ApiRequestError)
    expect((error as ApiRequestError).code).toBe('REQUEST_FAILED')
    expect((error as ApiRequestError).message).toBe('本地请求失败：未注册的调用 POST /api/mcp/restart')
    expect(invokeMock).not.toHaveBeenCalled()
  })

  it('POST rotate 收到空对象请求体时只传 sessionId', async () => {
    invokeMock.mockResolvedValue({})
    await apiFetch<unknown>('/api/mcp/clients/s-3/rotate', { method: 'POST', body: '{}' }, null)
    expect(invokeMock).toHaveBeenCalledWith('rotate_mcp_client_session_id', { sessionId: 's-3' })
  })
})
