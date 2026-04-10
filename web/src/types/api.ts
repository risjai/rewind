export interface Session {
  id: string
  name: string
  created_at: string
  updated_at: string
  status: 'Recording' | 'Completed' | 'Failed' | 'Forked'
  total_steps: number
  total_tokens: number
  metadata: Record<string, unknown>
}

export interface Timeline {
  id: string
  session_id: string
  parent_timeline_id: string | null
  fork_at_step: number | null
  created_at: string
  label: string
}

export interface SessionDetail {
  session: Session
  timelines: Timeline[]
}

export interface StepResponse {
  id: string
  timeline_id: string
  session_id: string
  step_number: number
  step_type: string
  step_type_label: string
  step_type_icon: string
  status: string
  created_at: string
  duration_ms: number
  tokens_in: number
  tokens_out: number
  model: string
  error: string | null
  response_preview: string
}

export interface StepDetail {
  id: string
  timeline_id: string
  session_id: string
  step_number: number
  step_type: string
  status: string
  created_at: string
  duration_ms: number
  tokens_in: number
  tokens_out: number
  model: string
  error: string | null
  request_body: unknown | null
  response_body: unknown | null
  messages: MessageView[] | null
}

export interface MessageView {
  role: string
  content: string
}

export interface Baseline {
  id: string
  name: string
  source_session_id: string
  source_timeline_id: string
  created_at: string
  description: string
  step_count: number
  total_tokens: number
  metadata: Record<string, unknown>
}

export interface BaselineStep {
  id: string
  baseline_id: string
  step_number: number
  step_type: string
  expected_status: string
  expected_model: string
  tokens_in: number
  tokens_out: number
  tool_name: string | null
  has_error: boolean
}

export interface BaselineDetail {
  baseline: Baseline
  steps: BaselineStep[]
}

export interface CacheStats {
  entries: number
  total_hits: number
  total_tokens_saved: number
}

export interface Snapshot {
  id: string
  session_id: string | null
  label: string
  directory: string
  blob_hash: string
  file_count: number
  size_bytes: number
  created_at: string
}

export interface TimelineDiff {
  diverge_at_step: number | null
  left_label: string
  right_label: string
  step_diffs: StepDiffEntry[]
}

export interface StepDiffEntry {
  step_number: number
  diff_type: 'Same' | 'Modified' | 'LeftOnly' | 'RightOnly'
  left: StepSummary | null
  right: StepSummary | null
}

export interface StepSummary {
  step_type: string
  status: string
  model: string
  tokens_in: number
  tokens_out: number
  duration_ms: number
  response_preview: string
}

export interface WsStepEvent {
  type: 'step'
  data: StepResponse
}

export interface WsSessionUpdate {
  type: 'session_update'
  data: {
    session_id: string
    status: string
    total_steps: number
    total_tokens: number
  }
}

export interface WsSubscribed {
  type: 'subscribed'
  session_id: string
}

export type WsServerMessage = WsStepEvent | WsSessionUpdate | WsSubscribed

// --- Eval types ---

export interface EvalDataset {
  id: string
  name: string
  description: string
  created_at: string
  updated_at: string
  version: number
  example_count: number
}

export interface DatasetExample {
  id: string
  ordinal: number
  input: unknown
  expected: unknown
  metadata: Record<string, unknown>
}

export interface EvalExperiment {
  id: string
  name: string
  dataset_id: string
  dataset_version: number
  status: 'pending' | 'running' | 'completed' | 'failed'
  created_at: string
  completed_at: string | null
  total_examples: number
  completed_examples: number
  avg_score: number | null
  min_score: number | null
  max_score: number | null
  pass_rate: number | null
  total_duration_ms: number
  total_tokens: number
  metadata: Record<string, unknown>
}

export interface ExperimentResultDetail {
  id: string
  ordinal: number
  output_preview: string
  status: string
  duration_ms: number
  error: string | null
  trace_session_id?: string | null
  scores: {
    evaluator_name: string
    score: number
    passed: boolean
    reasoning: string
  }[]
}

export interface ExperimentComparisonView {
  left_id: string
  left_name: string
  left_avg_score: number
  left_pass_rate: number
  right_id: string
  right_name: string
  right_avg_score: number
  right_pass_rate: number
  overall_delta: number
  regressions: number
  improvements: number
  unchanged: number
  example_diffs: {
    ordinal: number
    input_preview: string
    left_score: number
    right_score: number
    delta: number
    direction: 'regression' | 'improvement' | 'unchanged'
  }[]
}
