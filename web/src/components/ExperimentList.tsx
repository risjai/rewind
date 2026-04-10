import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn, timeAgo, formatDuration, formatTokens } from '@/lib/utils'
import { FlaskConical, Clock, Loader2, CheckCircle2, XCircle } from 'lucide-react'
import { ScoreBadge } from '@/components/ScoreBadge'
import { ExperimentDetail } from '@/components/ExperimentDetail'
import type { EvalExperiment } from '@/types/api'

export function ExperimentList() {
  const { selectedExperimentId, selectExperiment } = useStore()

  const { data: experiments = [], isLoading } = useQuery({
    queryKey: ['eval-experiments'],
    queryFn: () => api.evalExperiments(),
    refetchInterval: 5000,
  })

  const selectedExperiment = experiments.find((e) => e.id === selectedExperimentId) ?? null

  return (
    <div className="flex h-full">
      <div className="w-96 border-r border-neutral-800 flex flex-col">
        <div className="px-4 py-3 border-b border-neutral-800 flex items-center gap-2">
          <FlaskConical size={16} className="text-neutral-400" />
          <h2 className="text-sm font-semibold text-neutral-200">Experiments</h2>
        </div>
        <div className="flex-1 overflow-auto scrollbar-thin">
          {isLoading ? (
            <div className="text-center text-neutral-500 text-sm py-8">Loading...</div>
          ) : experiments.length === 0 ? (
            <div className="text-center text-neutral-500 text-sm py-8">
              <p>No experiments yet</p>
              <p className="text-xs mt-1">Run an experiment with the eval SDK</p>
            </div>
          ) : (
            <table className="w-full text-xs">
              <thead className="sticky top-0 bg-neutral-950">
                <tr className="text-neutral-500 border-b border-neutral-800">
                  <th className="text-left px-3 py-2 font-medium">Name</th>
                  <th className="text-left px-3 py-2 font-medium">Status</th>
                  <th className="text-right px-3 py-2 font-medium">Score</th>
                  <th className="text-right px-3 py-2 font-medium">Pass</th>
                  <th className="text-right px-3 py-2 font-medium">Time</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-neutral-800/50">
                {experiments.map((exp) => (
                  <ExperimentRow
                    key={exp.id}
                    experiment={exp}
                    selected={selectedExperimentId === exp.id}
                    onClick={() => selectExperiment(exp.id)}
                  />
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      <div className="flex-1">
        {selectedExperiment ? (
          <ExperimentDetail experiment={selectedExperiment} />
        ) : (
          <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
            Select an experiment to view results
          </div>
        )}
      </div>
    </div>
  )
}

function ExperimentRow({ experiment, selected, onClick }: { experiment: EvalExperiment; selected: boolean; onClick: () => void }) {
  return (
    <tr
      onClick={onClick}
      className={cn(
        'cursor-pointer transition-colors',
        selected ? 'bg-neutral-800/80 text-neutral-100' : 'text-neutral-300 hover:bg-neutral-900/60'
      )}
    >
      <td className="px-3 py-2">
        <div className="font-medium truncate max-w-[160px]">{experiment.name}</div>
        <div className="text-neutral-500 mt-0.5">
          {experiment.total_examples} examples · {timeAgo(experiment.created_at)}
        </div>
      </td>
      <td className="px-3 py-2">
        <StatusBadge status={experiment.status} />
      </td>
      <td className="px-3 py-2 text-right">
        <ScoreBadge score={experiment.avg_score} />
      </td>
      <td className="px-3 py-2 text-right">
        {experiment.pass_rate !== null ? (
          <span className={cn(
            'text-xs font-medium',
            experiment.pass_rate >= 0.8 ? 'text-green-400' : experiment.pass_rate >= 0.6 ? 'text-amber-400' : 'text-red-400'
          )}>
            {(experiment.pass_rate * 100).toFixed(0)}%
          </span>
        ) : (
          <span className="text-neutral-500">--</span>
        )}
      </td>
      <td className="px-3 py-2 text-right text-neutral-500">
        {experiment.total_duration_ms > 0 ? formatDuration(experiment.total_duration_ms) : '--'}
      </td>
    </tr>
  )
}

function StatusBadge({ status }: { status: EvalExperiment['status'] }) {
  const config: Record<EvalExperiment['status'], { icon: React.ReactNode; label: string; className: string }> = {
    pending: {
      icon: <Clock size={12} />,
      label: 'Pending',
      className: 'bg-neutral-800 text-neutral-400',
    },
    running: {
      icon: <Loader2 size={12} className="animate-spin" />,
      label: 'Running',
      className: 'bg-cyan-500/15 text-cyan-400',
    },
    completed: {
      icon: <CheckCircle2 size={12} />,
      label: 'Done',
      className: 'bg-green-500/15 text-green-400',
    },
    failed: {
      icon: <XCircle size={12} />,
      label: 'Failed',
      className: 'bg-red-500/15 text-red-400',
    },
  }

  const c = config[status]
  return (
    <span className={cn('inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-xs font-medium', c.className)}>
      {c.icon}
      {c.label}
    </span>
  )
}
