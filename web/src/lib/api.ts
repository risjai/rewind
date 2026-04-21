import type {
  Session, SessionDetail, StepResponse, StepDetail,
  Baseline, BaselineDetail, CacheStats, Snapshot,
  Timeline, TimelineDiff,
  EvalDataset, DatasetExample, EvalExperiment,
  ExperimentResultDetail, ExperimentComparisonView,
  SpanResponse,
} from '@/types/api'
import { getToken, promptForToken, clearToken } from '@/lib/auth'

const BASE = '/api'

function authHeaders(): HeadersInit {
  const token = getToken()
  return token ? { Authorization: `Bearer ${token}` } : {}
}

/**
 * Send a request with the stored token. On 401, prompt the user for a token
 * once and retry. If the retry still returns 401, the prompt token was also
 * wrong — clear it and surface the error.
 */
async function request(path: string, init: RequestInit = {}): Promise<Response> {
  const base = {
    ...init,
    headers: {
      ...(init.headers || {}),
      ...authHeaders(),
    },
  }
  let res = await fetch(`${BASE}${path}`, base)
  if (res.status !== 401) return res

  // Re-prompt and retry once.
  const tok = promptForToken()
  if (!tok) return res
  res = await fetch(`${BASE}${path}`, {
    ...init,
    headers: { ...(init.headers || {}), Authorization: `Bearer ${tok}` },
  })
  if (res.status === 401) clearToken()
  return res
}

async function get<T>(path: string): Promise<T> {
  const res = await request(path)
  if (!res.ok) {
    const text = await res.text()
    throw new Error(`API error ${res.status}: ${text}`)
  }
  return res.json()
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await request(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!res.ok) {
    const text = await res.text()
    throw new Error(`API error ${res.status}: ${text}`)
  }
  return res.json()
}

export const api = {
  health: () => get<{ status: string; version: string }>('/health'),
  sessions: () => get<Session[]>('/sessions'),
  session: (id: string) => get<SessionDetail>(`/sessions/${id}`),
  sessionSteps: (id: string, timeline?: string) => {
    const q = timeline ? `?timeline=${encodeURIComponent(timeline)}` : ''
    return get<StepResponse[]>(`/sessions/${id}/steps${q}`)
  },
  sessionTimelines: (id: string) => get<Timeline[]>(`/sessions/${id}/timelines`),
  stepDetail: (id: string) => get<StepDetail>(`/steps/${id}`),
  diffTimelines: (sessionId: string, left: string, right: string) =>
    get<TimelineDiff>(`/sessions/${sessionId}/diff?left=${encodeURIComponent(left)}&right=${encodeURIComponent(right)}`),
  baselines: () => get<Baseline[]>('/baselines'),
  baseline: (name: string) => get<BaselineDetail>(`/baselines/${name}`),
  cacheStats: () => get<CacheStats>('/cache/stats'),
  snapshots: () => get<Snapshot[]>('/snapshots'),

  // Eval
  evalDatasets: () => get<EvalDataset[]>('/eval/datasets'),
  evalDataset: (name: string) => get<{ dataset: EvalDataset; examples: DatasetExample[] }>(`/eval/datasets/${name}`),
  evalExperiments: (dataset?: string) => get<EvalExperiment[]>(`/eval/experiments${dataset ? `?dataset=${dataset}` : ''}`),
  evalExperiment: (id: string) => get<EvalExperiment>(`/eval/experiments/${id}`),
  evalExperimentResults: (id: string) => get<ExperimentResultDetail[]>(`/eval/experiments/${id}/results`),
  evalCompare: (left: string, right: string) => get<ExperimentComparisonView>(`/eval/compare?left=${left}&right=${right}`),

  // Spans
  sessionSpans: (id: string, timeline?: string) => {
    const q = timeline ? `?timeline=${encodeURIComponent(timeline)}` : ''
    return get<SpanResponse[]>(`/sessions/${id}/spans${q}`)
  },

  // OTel Export
  exportOtel: (sessionId: string, opts: { include_content?: boolean; timeline_id?: string | null; all_timelines?: boolean } = {}) =>
    post<{ spans_exported: number; trace_id: string }>(`/sessions/${sessionId}/export/otel`, opts),
}
