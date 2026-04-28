import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Play, Loader2, CheckCircle2, AlertCircle, Rocket, X } from 'lucide-react'
import { api } from '@/lib/api'
import { useReplayJob } from '@/hooks/use-replay-job'

/**
 * Dashboard "Run replay" button (Phase 3 commit 8/13).
 *
 * Renders inline near the fork-button on the session-detail and
 * fork-timeline views. Clicking opens a modal that:
 *   1. Lists active runners (filtered to status=active).
 *   2. Lets the operator pick one + confirm a replay from a step.
 *   3. POSTs /api/sessions/{sid}/replay-jobs (shape A: server
 *      forks + creates context + dispatches).
 *   4. Streams progress over WebSocket via useReplayJob.
 *
 * If no runners are registered, the modal shows the CLI fallback
 * command (existing `rewind replay` flow).
 */
export interface RunReplayButtonProps {
  sessionId: string
  sourceTimelineId: string
  atStep: number
}

export function RunReplayButton({
  sessionId,
  sourceTimelineId,
  atStep,
}: RunReplayButtonProps) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button
        onClick={() => setOpen(true)}
        className="flex items-center gap-1.5 px-2.5 py-1 text-xs bg-cyan-700 hover:bg-cyan-600 text-white rounded transition-colors"
        title="Dispatch this replay to a registered runner"
      >
        <Play size={12} /> Run replay
      </button>
      {open && (
        <ReplayJobModal
          sessionId={sessionId}
          sourceTimelineId={sourceTimelineId}
          atStep={atStep}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  )
}

interface ReplayJobModalProps {
  sessionId: string
  sourceTimelineId: string
  atStep: number
  onClose: () => void
}

export function ReplayJobModal({
  sessionId,
  sourceTimelineId,
  atStep,
  onClose,
}: ReplayJobModalProps) {
  const { data: runners = [], isLoading } = useQuery({
    queryKey: ['runners'],
    queryFn: api.runners,
  })
  const activeRunners = runners.filter((r) => r.status === 'active')
  const [runnerId, setRunnerId] = useState<string>(activeRunners[0]?.id ?? '')
  const [strictMatch, setStrictMatch] = useState(false)

  const {
    dispatch,
    isDispatching,
    dispatchError,
    job,
    reset,
  } = useReplayJob(sessionId)

  // Update default runner once data loads.
  if (!runnerId && activeRunners.length > 0) {
    setRunnerId(activeRunners[0].id)
  }

  const submit = () => {
    if (!runnerId) return
    dispatch({
      runner_id: runnerId,
      source_timeline_id: sourceTimelineId,
      at_step: atStep,
      strict_match: strictMatch,
    })
  }

  const handleClose = () => {
    reset()
    onClose()
  }

  // Disable close affordances mid-dispatch so the user can't drop
  // an in-flight job by accident. The Job-progress view has its own
  // explicit Close button; the modal still closes from any state via
  // the JobProgressView once the job reaches a terminal state.
  const closeDisabled = isDispatching

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Run replay on a registered runner"
      className="fixed inset-0 bg-black/60 flex items-center justify-center z-50 p-4"
      onClick={() => {
        if (!closeDisabled) handleClose()
      }}
    >
      <div
        className="relative bg-neutral-900 border border-neutral-700 rounded-xl shadow-2xl w-full max-w-lg mx-4"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-neutral-800">
          <div className="flex items-center gap-2">
            <Rocket size={16} className="text-emerald-400" />
            <h3 className="text-sm font-semibold text-neutral-200">
              Run replay from step #{atStep}
            </h3>
          </div>
          <button
            onClick={handleClose}
            disabled={closeDisabled}
            aria-label="Close"
            className="text-neutral-500 hover:text-neutral-300 transition-colors disabled:opacity-50"
          >
            <X size={16} />
          </button>
        </div>

        <div className="px-5 py-4 space-y-4">
          <p className="text-xs text-neutral-500">
            Forks at step {atStep} on the selected timeline and
            dispatches a replay job to a registered runner. Progress
            streams here live via WebSocket.
          </p>

          {isLoading ? (
            <div className="text-center py-6 text-neutral-500 text-sm">
              Loading runners...
            </div>
          ) : activeRunners.length === 0 ? (
            <NoRunnersFallback sessionId={sessionId} atStep={atStep} />
          ) : !job ? (
            <RunnerPickForm
              runners={activeRunners}
              runnerId={runnerId}
              onRunnerChange={setRunnerId}
              strictMatch={strictMatch}
              onStrictMatchChange={setStrictMatch}
              onSubmit={submit}
              isDispatching={isDispatching}
              error={dispatchError?.message ?? null}
            />
          ) : (
            <JobProgressView job={job} onClose={handleClose} />
          )}
        </div>
      </div>
    </div>
  )
}

function RunnerPickForm({
  runners,
  runnerId,
  onRunnerChange,
  strictMatch,
  onStrictMatchChange,
  onSubmit,
  isDispatching,
  error,
}: {
  runners: { id: string; name: string }[]
  runnerId: string
  onRunnerChange: (id: string) => void
  strictMatch: boolean
  onStrictMatchChange: (v: boolean) => void
  onSubmit: () => void
  isDispatching: boolean
  error: string | null
}) {
  return (
    <div className="space-y-3">
      <div>
        <label className="block text-xs text-neutral-400 mb-1">
          Runner
        </label>
        <select
          value={runnerId}
          onChange={(e) => onRunnerChange(e.target.value)}
          className="w-full px-2 py-1.5 bg-neutral-800 border border-neutral-700 rounded text-sm text-neutral-200"
        >
          {runners.map((r) => (
            <option key={r.id} value={r.id}>
              {r.name} ({r.id.slice(0, 8)})
            </option>
          ))}
        </select>
      </div>
      <label className="flex items-center gap-2 text-xs text-neutral-400">
        <input
          type="checkbox"
          checked={strictMatch}
          onChange={(e) => onStrictMatchChange(e.target.checked)}
        />
        <span>
          Strict cache match
          <span className="text-neutral-600">
            {' '}
            (HTTP 409 on body divergence; otherwise warn-only)
          </span>
        </span>
      </label>
      {error && (
        <div className="text-xs text-red-400 bg-red-950/40 border border-red-800 rounded p-2">
          {error}
        </div>
      )}
      <div className="flex justify-end gap-2 pt-2">
        <button
          onClick={onSubmit}
          disabled={isDispatching || !runnerId}
          className="flex items-center gap-1.5 px-3 py-1.5 text-sm bg-cyan-600 hover:bg-cyan-500 disabled:bg-neutral-700 text-white rounded"
        >
          {isDispatching ? (
            <>
              <Loader2 size={14} className="animate-spin" /> Dispatching...
            </>
          ) : (
            <>
              <Play size={14} /> Dispatch
            </>
          )}
        </button>
      </div>
    </div>
  )
}

function NoRunnersFallback({
  sessionId,
  atStep,
}: {
  sessionId: string
  atStep: number
}) {
  return (
    <div className="space-y-3">
      <p className="text-sm text-neutral-300">
        No active runners registered.
      </p>
      <p className="text-xs text-neutral-500">
        Register one on the{' '}
        <a
          href="#"
          onClick={(e) => {
            e.preventDefault()
            window.location.hash = '#/runners'
          }}
          className="text-cyan-400 hover:underline"
        >
          Runners page
        </a>
        , or run this replay locally via the CLI:
      </p>
      <pre className="px-3 py-2 bg-black border border-neutral-700 rounded text-xs text-cyan-300 overflow-x-auto">
        rewind replay {sessionId.slice(0, 8)}... --from {atStep}
      </pre>
    </div>
  )
}

function JobProgressView({
  job,
  onClose,
}: {
  job: NonNullable<ReturnType<typeof useReplayJob>['job']>
  onClose: () => void
}) {
  const isTerminal = job.state === 'completed' || job.state === 'errored'
  const Icon =
    job.state === 'completed'
      ? CheckCircle2
      : job.state === 'errored'
        ? AlertCircle
        : Loader2
  const iconCls =
    job.state === 'completed'
      ? 'text-green-400'
      : job.state === 'errored'
        ? 'text-red-400'
        : 'text-cyan-400 animate-spin'

  const pct =
    job.progress_total && job.progress_total > 0
      ? Math.min(100, Math.round((job.progress_step / job.progress_total) * 100))
      : null

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-2">
        <Icon size={18} className={iconCls} />
        <div className="flex-1">
          <div className="text-sm font-semibold text-neutral-200 capitalize">
            {job.state.replace('_', ' ')}
          </div>
          <div className="text-xs text-neutral-500 font-mono">
            job {job.job_id.slice(0, 8)}
          </div>
        </div>
      </div>

      {job.fork_timeline_id && (
        <div className="text-xs text-neutral-500">
          Fork timeline:{' '}
          <code className="text-cyan-300">
            {job.fork_timeline_id.slice(0, 8)}
          </code>
        </div>
      )}

      <div className="space-y-1">
        <div className="flex justify-between text-xs text-neutral-400">
          <span>
            Progress: step {job.progress_step}
            {job.progress_total !== null && ` / ${job.progress_total}`}
          </span>
          {pct !== null && <span>{pct}%</span>}
        </div>
        <div className="h-2 bg-neutral-800 rounded overflow-hidden">
          <div
            className={`h-full transition-all ${job.state === 'errored' ? 'bg-red-500' : 'bg-cyan-500'}`}
            style={{ width: pct !== null ? `${pct}%` : '20%' }}
          />
        </div>
      </div>

      {job.error_message && (
        <div className="text-xs text-red-400 bg-red-950/40 border border-red-800 rounded p-2">
          <div className="font-semibold">Error ({job.error_stage}):</div>
          <div>{job.error_message}</div>
        </div>
      )}

      <div className="flex justify-end gap-2 pt-2">
        <button
          onClick={onClose}
          className="px-3 py-1.5 text-sm bg-neutral-700 hover:bg-neutral-600 text-white rounded"
        >
          {isTerminal ? 'Close' : 'Close (job continues in background)'}
        </button>
      </div>
    </div>
  )
}
