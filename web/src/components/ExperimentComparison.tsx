import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { cn } from '@/lib/utils'
import { useState } from 'react'
import { GitCompareArrows, ArrowUp, ArrowDown, Minus } from 'lucide-react'
import { ScoreBadge } from '@/components/ScoreBadge'
import type { EvalExperiment, ExperimentComparisonView } from '@/types/api'

export function ExperimentComparison() {
  const [leftId, setLeftId] = useState<string>('')
  const [rightId, setRightId] = useState<string>('')

  const { data: experiments = [] } = useQuery({
    queryKey: ['eval-experiments'],
    queryFn: () => api.evalExperiments(),
  })

  const { data: comparison, isLoading, isError } = useQuery({
    queryKey: ['eval-compare', leftId, rightId],
    queryFn: () => api.evalCompare(leftId, rightId),
    enabled: !!leftId && !!rightId && leftId !== rightId,
  })

  return (
    <div className="flex flex-col h-full">
      <div className="px-4 py-3 border-b border-neutral-800">
        <div className="flex items-center gap-2 mb-3">
          <GitCompareArrows size={16} className="text-neutral-400" />
          <h2 className="text-sm font-semibold text-neutral-200">Compare Experiments</h2>
        </div>
        <div className="flex items-center gap-3">
          <ExperimentSelect
            label="Left (baseline)"
            value={leftId}
            onChange={setLeftId}
            experiments={experiments}
            excludeId={rightId}
          />
          <span className="text-neutral-600 text-sm">vs</span>
          <ExperimentSelect
            label="Right (new)"
            value={rightId}
            onChange={setRightId}
            experiments={experiments}
            excludeId={leftId}
          />
        </div>
      </div>

      <div className="flex-1 overflow-auto scrollbar-thin">
        {!leftId || !rightId ? (
          <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
            Select two experiments to compare
          </div>
        ) : leftId === rightId ? (
          <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
            Select two different experiments
          </div>
        ) : isLoading ? (
          <div className="text-center text-neutral-500 text-sm py-8">Loading comparison...</div>
        ) : isError ? (
          <div className="text-center text-red-400 text-sm py-8">Failed to load comparison</div>
        ) : comparison ? (
          <ComparisonResults comparison={comparison} />
        ) : null}
      </div>
    </div>
  )
}

function ExperimentSelect({
  label, value, onChange, experiments, excludeId,
}: {
  label: string
  value: string
  onChange: (v: string) => void
  experiments: EvalExperiment[]
  excludeId: string
}) {
  return (
    <div className="flex-1">
      <label className="block text-[10px] uppercase tracking-wider text-neutral-500 mb-1">{label}</label>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full bg-neutral-900 border border-neutral-700 text-neutral-200 text-xs rounded px-2.5 py-1.5 focus:outline-none focus:border-neutral-500 transition-colors"
      >
        <option value="">Select experiment...</option>
        {experiments
          .filter((e) => e.id !== excludeId)
          .map((e) => (
            <option key={e.id} value={e.id}>
              {e.name} {e.avg_score !== null ? `(${e.avg_score.toFixed(2)})` : ''}
            </option>
          ))}
      </select>
    </div>
  )
}

function ComparisonResults({ comparison }: { comparison: ExperimentComparisonView }) {
  return (
    <div>
      {/* Summary header */}
      <div className="px-4 py-4 border-b border-neutral-800 bg-neutral-900/40">
        <div className="flex items-center gap-8">
          <div>
            <div className="text-[10px] uppercase tracking-wider text-neutral-500">Overall Delta</div>
            <div className={cn(
              'text-lg font-bold mt-0.5',
              comparison.overall_delta > 0 ? 'text-green-400' : comparison.overall_delta < 0 ? 'text-red-400' : 'text-neutral-400'
            )}>
              {comparison.overall_delta > 0 ? '+' : ''}{comparison.overall_delta.toFixed(3)}
            </div>
          </div>
          <div className="flex items-center gap-6 text-xs">
            <SummaryChip
              count={comparison.improvements}
              label="improvements"
              color="text-green-400"
              icon={<ArrowUp size={12} />}
            />
            <SummaryChip
              count={comparison.regressions}
              label="regressions"
              color="text-red-400"
              icon={<ArrowDown size={12} />}
            />
            <SummaryChip
              count={comparison.unchanged}
              label="unchanged"
              color="text-neutral-400"
              icon={<Minus size={12} />}
            />
          </div>
        </div>

        <div className="flex items-center gap-8 mt-3 text-xs">
          <div>
            <span className="text-neutral-500">Left: </span>
            <span className="text-neutral-300 font-medium">{comparison.left_name}</span>
            <span className="text-neutral-500 ml-1">(avg {comparison.left_avg_score.toFixed(2)}, pass {(comparison.left_pass_rate * 100).toFixed(0)}%)</span>
          </div>
          <div>
            <span className="text-neutral-500">Right: </span>
            <span className="text-neutral-300 font-medium">{comparison.right_name}</span>
            <span className="text-neutral-500 ml-1">(avg {comparison.right_avg_score.toFixed(2)}, pass {(comparison.right_pass_rate * 100).toFixed(0)}%)</span>
          </div>
        </div>
      </div>

      {/* Per-example diff table */}
      <table className="w-full text-xs">
        <thead className="sticky top-0 bg-neutral-950">
          <tr className="text-neutral-500 border-b border-neutral-800">
            <th className="text-left px-4 py-2 font-medium w-12">#</th>
            <th className="text-left px-4 py-2 font-medium">Input</th>
            <th className="text-right px-4 py-2 font-medium">Left</th>
            <th className="text-right px-4 py-2 font-medium">Right</th>
            <th className="text-right px-4 py-2 font-medium">Delta</th>
            <th className="text-center px-4 py-2 font-medium w-12">Dir</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-neutral-800/50">
          {comparison.example_diffs.map((diff) => (
            <tr key={diff.ordinal} className="text-neutral-300 hover:bg-neutral-900/60">
              <td className="px-4 py-2 font-mono text-neutral-500">{diff.ordinal}</td>
              <td className="px-4 py-2 max-w-xs">
                <span className="block truncate">{diff.input_preview || '--'}</span>
              </td>
              <td className="px-4 py-2 text-right">
                <ScoreBadge score={diff.left_score} />
              </td>
              <td className="px-4 py-2 text-right">
                <ScoreBadge score={diff.right_score} />
              </td>
              <td className="px-4 py-2 text-right">
                <span className={cn(
                  'text-xs font-medium',
                  diff.delta > 0 ? 'text-green-400' : diff.delta < 0 ? 'text-red-400' : 'text-neutral-500'
                )}>
                  {diff.delta > 0 ? '+' : ''}{diff.delta.toFixed(3)}
                </span>
              </td>
              <td className="px-4 py-2 text-center">
                <DirectionIcon direction={diff.direction} />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function SummaryChip({ count, label, color, icon }: { count: number; label: string; color: string; icon: React.ReactNode }) {
  return (
    <div className={cn('flex items-center gap-1', color)}>
      {icon}
      <span className="font-semibold">{count}</span>
      <span className="text-neutral-500">{label}</span>
    </div>
  )
}

function DirectionIcon({ direction }: { direction: 'regression' | 'improvement' | 'unchanged' }) {
  switch (direction) {
    case 'improvement':
      return <span className="text-green-400 font-bold" title="Improvement">&#9650;</span>
    case 'regression':
      return <span className="text-red-400 font-bold" title="Regression">&#9660;</span>
    case 'unchanged':
      return <span className="text-neutral-500" title="Unchanged">&#9552;</span>
  }
}
