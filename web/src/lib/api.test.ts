import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { api } from './api'
import { clearToken, setToken } from './auth'

const mockFetch = vi.fn()
vi.stubGlobal('fetch', mockFetch)

function mockJsonResponse(data: unknown, status = 200) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: () => Promise.resolve(data),
    text: () => Promise.resolve(JSON.stringify(data)),
  }
}

// `api.ts` always threads requests through `request()`, which adds an
// Authorization header when a token is stored and otherwise passes an empty
// headers object. These helpers match that exact call shape.
function expectUnauthedCall(path: string, extra: Record<string, unknown> = {}) {
  expect(mockFetch).toHaveBeenCalledWith(path, { headers: {}, ...extra })
}
function expectAuthedCall(path: string, token: string, extra: Record<string, unknown> = {}) {
  expect(mockFetch).toHaveBeenCalledWith(path, {
    headers: { Authorization: `Bearer ${token}` },
    ...extra,
  })
}

beforeEach(() => {
  mockFetch.mockReset()
  clearToken()
})

afterEach(() => {
  clearToken()
})

describe('api.health', () => {
  it('calls /api/health and returns parsed JSON', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse({ status: 'ok', version: '0.2.0' }))
    const result = await api.health()
    expectUnauthedCall('/api/health')
    expect(result).toEqual({ status: 'ok', version: '0.2.0' })
  })
})

describe('api.sessions', () => {
  it('calls /api/sessions', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    const result = await api.sessions()
    expectUnauthedCall('/api/sessions')
    expect(result).toEqual([])
  })
})

describe('api.session', () => {
  it('calls /api/sessions/:id', async () => {
    const data = { session: { id: 'abc' }, timelines: [] }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.session('abc')
    expectUnauthedCall('/api/sessions/abc')
    expect(result).toEqual(data)
  })
})

describe('api.sessionSteps', () => {
  it('calls without timeline param', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    await api.sessionSteps('abc')
    expectUnauthedCall('/api/sessions/abc/steps')
  })

  it('includes timeline query param', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    await api.sessionSteps('abc', 'main')
    expectUnauthedCall('/api/sessions/abc/steps?timeline=main')
  })
})

describe('api.stepDetail', () => {
  it('calls /api/steps/:id', async () => {
    const data = { id: 'step1', step_number: 1 }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.stepDetail('step1')
    expectUnauthedCall('/api/steps/step1')
    expect(result).toEqual(data)
  })
})

describe('api.diffTimelines', () => {
  it('calls with left and right params', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse({ step_diffs: [] }))
    await api.diffTimelines('sess1', 'left-id', 'right-id')
    expectUnauthedCall('/api/sessions/sess1/diff?left=left-id&right=right-id')
  })
})

describe('api.baselines', () => {
  it('calls /api/baselines', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    const result = await api.baselines()
    expectUnauthedCall('/api/baselines')
    expect(result).toEqual([])
  })
})

describe('api.baseline', () => {
  it('calls /api/baselines/:name', async () => {
    const data = { baseline: { name: 'test' }, steps: [] }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.baseline('test')
    expectUnauthedCall('/api/baselines/test')
    expect(result).toEqual(data)
  })
})

describe('api.cacheStats', () => {
  it('calls /api/cache/stats', async () => {
    const data = { entries: 5, total_hits: 10, total_tokens_saved: 1000 }
    mockFetch.mockResolvedValue(mockJsonResponse(data))
    const result = await api.cacheStats()
    expectUnauthedCall('/api/cache/stats')
    expect(result).toEqual(data)
  })
})

describe('api.snapshots', () => {
  it('calls /api/snapshots', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    const result = await api.snapshots()
    expectUnauthedCall('/api/snapshots')
    expect(result).toEqual([])
  })
})

describe('error handling', () => {
  it('throws on non-OK response', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse('Not found', 404))
    await expect(api.sessions()).rejects.toThrow('API error 404')
  })

  it('throws on 500 response', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse('Internal error', 500))
    await expect(api.health()).rejects.toThrow('API error 500')
  })
})

describe('auth token injection', () => {
  it('includes Authorization: Bearer when a token is stored', async () => {
    setToken('stored-token-abc')
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    await api.sessions()
    expectAuthedCall('/api/sessions', 'stored-token-abc')
  })

  it('sends POST with Authorization header when authed', async () => {
    setToken('stored-token-abc')
    mockFetch.mockResolvedValue(mockJsonResponse({ spans_exported: 0, trace_id: 'x' }))
    await api.exportOtel('sess1', { include_content: false })
    expect(mockFetch).toHaveBeenCalledWith('/api/sessions/sess1/export/otel', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Authorization: 'Bearer stored-token-abc',
      },
      body: JSON.stringify({ include_content: false }),
    })
  })

  it('passes through when no token is stored (loopback default)', async () => {
    mockFetch.mockResolvedValue(mockJsonResponse([]))
    await api.sessions()
    expectUnauthedCall('/api/sessions')
  })
})
