import type {
  Session, SessionDetail, StepResponse, StepDetail,
  Baseline, BaselineDetail, CacheStats, Snapshot,
  Timeline, TimelineDiff,
  ForkResponse, CreateReplayContextResponse, DeleteReplayContextResponse, DeleteTimelineResponse,
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

async function del<T>(path: string): Promise<T> {
  const res = await request(path, { method: 'DELETE' })
  if (!res.ok) {
    const text = await res.text()
    throw new Error(`API error ${res.status}: ${text}`)
  }
  return res.json()
}

export const api = {
  health: () => get<{ status: string; version: string; allow_main_edits: boolean }>('/health'),
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

  // Fork / Replay
  forkSession: (sessionId: string, body: { at_step: number; label: string; timeline_id?: string }) =>
    post<ForkResponse>(`/sessions/${sessionId}/fork`, body),
  createReplayContext: (body: { session_id: string; from_step: number; fork_timeline_id: string }) =>
    post<CreateReplayContextResponse>('/replay-contexts', body),
  deleteReplayContext: (id: string) =>
    del<DeleteReplayContextResponse>(`/replay-contexts/${id}`),
  deleteTimeline: (sessionId: string, timelineId: string) =>
    del<DeleteTimelineResponse>(`/sessions/${sessionId}/timelines/${timelineId}`),

  // Step editing
  patchStep: (stepId: string, body: { request_body?: unknown; response_body?: unknown; target_timeline_id?: string }) =>
    request(`/steps/${stepId}/edit`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    }).then(async (res) => {
      if (!res.ok) throw new Error(`API error ${res.status}: ${await res.text()}`)
      return res.json() as Promise<{ step_id: string; resolved_step_id: string; deleted_downstream_count: number }>
    }),
  cascadeCount: (stepId: string, targetTimelineId?: string) => {
    const q = targetTimelineId ? `?target_timeline_id=${encodeURIComponent(targetTimelineId)}` : ''
    return get<{ deleted_downstream_count: number; on_main: boolean }>(`/steps/${stepId}/cascade-count${q}`)
  },
  forkAndEditStep: (sessionId: string, body: {
    source_timeline_id: string;
    at_step: number;
    request_body?: unknown;
    response_body?: unknown;
    label?: string;
  }) => post<{ fork_timeline_id: string; step_id: string }>(`/sessions/${sessionId}/fork-and-edit-step`, body),

  // Phase 3 commit 7/13: runner registry
  runners: () => get<RunnerView[]>('/runners'),
  runner: (id: string) => get<RunnerView>(`/runners/${id}`),
  registerRunner: (body: { name: string; mode: 'webhook'; webhook_url: string }) =>
    post<RegisterRunnerResponse>('/runners', body),
  removeRunner: (id: string) =>
    request(`/runners/${id}`, { method: 'DELETE' }).then(async (res) => {
      if (res.status === 204) return { ok: true } as const
      const body = await res.json().catch(() => ({}))
      throw new Error(body.error || `delete failed: ${res.status}`)
    }),
  regenerateRunnerToken: (id: string) =>
    post<RegisterRunnerResponse>(`/runners/${id}/regenerate-token`, {}),

  // Phase 3 commit 8/13: replay-job dispatch + introspection
  createReplayJob: (sessionId: string, body: CreateReplayJobBody) =>
    post<CreateReplayJobResponse>(`/sessions/${sessionId}/replay-jobs`, body),
  listReplayJobsForSession: (sessionId: string) =>
    get<ReplayJobView[]>(`/sessions/${sessionId}/replay-jobs`),
  replayJob: (jobId: string) => get<ReplayJobView>(`/replay-jobs/${jobId}`),
  // Review #154 N1: cancellation removed. The plan defers cooperative
  // cancel to v3.1; the UI no longer surfaces a button. Operators
  // who must abandon a runaway runner kill the runner process; the
  // lease reaper marks the job errored ~5 min later.
}

// ──────────────────────────────────────────────────────────────────
// Phase 3 commit 7+8 types (kept here so the runners page + the
// dispatch button share a single source of truth without yet
// touching the broader types/api.ts file)
// ──────────────────────────────────────────────────────────────────

export interface RunnerView {
  id: string
  name: string
  mode: 'webhook' | 'polling'
  webhook_url: string | null
  auth_token_preview: string
  status: 'active' | 'disabled' | 'stale'
  created_at: string
  last_seen_at: string | null
}

export interface RegisterRunnerResponse {
  runner: RunnerView
  raw_token: string
  raw_token_warning: string
}

export type CreateReplayJobBody =
  | {
      runner_id: string
      source_timeline_id: string
      at_step: number
      strict_match?: boolean
    }
  | {
      runner_id: string
      replay_context_id: string
    }

export interface CreateReplayJobResponse {
  job_id: string
  replay_context_id: string
  fork_timeline_id?: string | null
  state: string
  dispatch_deadline_at?: string | null
}

export interface ReplayJobView {
  id: string
  runner_id: string | null
  session_id: string
  replay_context_id: string | null
  state: string
  error_message: string | null
  error_stage: string | null
  progress_step: number
  progress_total: number | null
  created_at: string
  dispatched_at: string | null
  started_at: string | null
  completed_at: string | null
  dispatch_deadline_at: string | null
  lease_expires_at: string | null
}
