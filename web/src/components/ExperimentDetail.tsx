import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import { useState } from 'react'
import { CheckCircle2, XCircle, ChevronDown, ChevronRight, ExternalLink } from 'lucide-react'
import { ScoreBadge } from '@/components/ScoreBadge'
import type { EvalExperiment, ExperimentResultDetail } from '@/types/api'

interface ExperimentDetailProps {
  experiment: EvalExperiment
}

export function ExperimentDetail({ experiment }: ExperimentDetailProps) {
  const { data: results = [], isLoading } = useQuery({
    queryKey: ['eval-experiment-results', experiment.id],
    queryFn: () => api.evalExperimentResults(experiment.id),
    refetchInterval: experiment.status === 'running' ? 3000 : false,
  })

  return (
    <div className="flex flex-col h-full">
      <div className="px-4 py-3 border-b border-neutral-800">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-neutral-200">{experiment.name}</h3>
          <StatusPill status={experiment.status} />
        </div>
        <div className="flex items-center gap-4 text-xs text-neutral-500 mt-1.5">
          <span>Dataset v{experiment.dataset_version}</span>
          <span>{experiment.completed_examples}/{experiment.total_examples} examples</span>
          {experiment.total_duration_ms > 0 && <span>{formatDuration(experiment.total_duration_ms)}</span>}
          {experiment.total_tokens > 0 && <span>{formatTokens(experiment.total_tokens)} tokens</span>}
        </div>

        <div className="flex items-center gap-6 mt-3">
          <AggStat label="Avg Score" value={experiment.avg_score} />
          <AggStat label="Min" value={experiment.min_score} />
          <AggStat label="Max" value={experiment.max_score} />
          <div>
            <div className="text-[10px] uppercase tracking-wider text-neutral-500">Pass Rate</div>
            <div className={cn(
              'text-sm font-semibold mt-0.5',
              experiment.pass_rate !== null
                ? experiment.pass_rate >= 0.8 ? 'text-green-400' : experiment.pass_rate >= 0.6 ? 'text-amber-400' : 'text-red-400'
                : 'text-neutral-500'
            )}>
              {experiment.pass_rate !== null ? `${(experiment.pass_rate * 100).toFixed(1)}%` : '--'}
            </div>
          </div>
        </div>
      </div>

      <div className="flex-1 overflow-auto scrollbar-thin">
        {isLoading ? (
          <div className="text-center text-neutral-500 text-sm py-8">Loading results...</div>
        ) : results.length === 0 ? (
          <div className="text-center text-neutral-500 text-sm py-8">No results yet</div>
        ) : (
          <table className="w-full text-xs">
            <thead className="sticky top-0 bg-neutral-950">
              <tr className="text-neutral-500 border-b border-neutral-800">
                <th className="text-left px-4 py-2 font-medium w-12">#</th>
                <th className="text-left px-4 py-2 font-medium w-12">Status</th>
                <th className="text-left px-4 py-2 font-medium">Output</th>
                <th className="text-right px-4 py-2 font-medium">Score</th>
                <th className="text-right px-4 py-2 font-medium">Duration</th>
                <th className="text-left px-4 py-2 font-medium w-8"></th>
              </tr>
            </thead>
            <tbody className="divide-y divide-neutral-800/50">
              {results.map((r) => (
                <ResultRow key={r.id} result={r} />
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}

function ResultRow({ result }: { result: ExperimentResultDetail }) {
  const [expanded, setExpanded] = useState(false)
  const avgScore = result.scores.length > 0
    ? result.scores.reduce((sum, s) => sum + s.score, 0) / result.scores.length
    : null
  const hasError = result.status === 'error' || !!result.error
  const traceSessionId = result.trace_session_id ?? undefined

  return (
    <>
      <tr
        onClick={() => setExpanded(!expanded)}
        className={cn(
          'cursor-pointer transition-colors',
          hasError ? 'text-red-300' : 'text-neutral-300',
          'hover:bg-neutral-900/60'
        )}
      >
        <td className="px-4 py-2 font-mono text-neutral-500">{result.ordinal}</td>
        <td className="px-4 py-2">
          {hasError ? (
            <XCircle size={14} className="text-red-400" />
          ) : (
            <CheckCircle2 size={14} className="text-green-400" />
          )}
        </td>
        <td className="px-4 py-2 max-w-md">
          <span className="block truncate">{result.output_preview || '--'}</span>
          {result.error && (
            <span className="block truncate text-red-400 mt-0.5">{result.error}</span>
          )}
        </td>
        <td className="px-4 py-2 text-right">
          <ScoreBadge score={avgScore} />
        </td>
        <td className="px-4 py-2 text-right text-neutral-500">
          {formatDuration(result.duration_ms)}
        </td>
        <td className="px-4 py-2">
          <span className="text-neutral-600">
            {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </span>
        </td>
      </tr>
      {expanded && (
        <tr>
          <td colSpan={6} className="bg-neutral-900/40 px-4 py-3">
            {traceSessionId && (
              <div className="mb-3">
                <a
                  href={`#/session/${traceSessionId}`}
                  className="inline-flex items-center gap-1 text-xs text-cyan-400 hover:text-cyan-300 transition-colors"
                >
                  <ExternalLink size={12} />
                  View Rewind session
                </a>
              </div>
            )}
            {result.scores.length === 0 ? (
              <p className="text-neutral-500 text-xs">No evaluator scores</p>
            ) : (
              <div className="space-y-2">
                {result.scores.map((s, i) => (
                  <div key={i} className="rounded bg-neutral-800/60 px-3 py-2">
                    <div className="flex items-center justify-between">
                      <div className="flex items-center gap-2">
                        <span className="text-xs font-medium text-neutral-300">{s.evaluator_name}</span>
                        <ScoreBadge score={s.score} />
                        {s.passed ? (
                          <CheckCircle2 size={12} className="text-green-400" />
                        ) : (
                          <XCircle size={12} className="text-red-400" />
                        )}
                      </div>
                    </div>
                    {s.reasoning && (
                      <p className="text-xs text-neutral-400 mt-1.5 leading-relaxed">{s.reasoning}</p>
                    )}
                  </div>
                ))}
              </div>
            )}
          </td>
        </tr>
      )}
    </>
  )
}

function AggStat({ label, value }: { label: string; value: number | null }) {
  return (
    <div>
      <div className="text-[10px] uppercase tracking-wider text-neutral-500">{label}</div>
      <div className="mt-0.5">
        <ScoreBadge score={value} />
      </div>
    </div>
  )
}

function StatusPill({ status }: { status: EvalExperiment['status'] }) {
  const styles: Record<EvalExperiment['status'], string> = {
    pending: 'bg-neutral-800 text-neutral-400',
    running: 'bg-cyan-500/15 text-cyan-400',
    completed: 'bg-green-500/15 text-green-400',
    failed: 'bg-red-500/15 text-red-400',
  }

  return (
    <span className={cn('inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium capitalize', styles[status])}>
      {status}
    </span>
  )
}
