import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn, formatTokens, formatDuration } from '@/lib/utils'
import { useState, useEffect } from 'react'
import { ArrowLeft, Equal, Diff, ArrowLeftRight } from 'lucide-react'
import type { Timeline, TimelineDiff, StepDiffEntry } from '@/types/api'

// Reads left/right timeline IDs from the URL hash, if any.
// Hash shape: `#/diff/{sessionId}/{leftId}/{rightId}` — set by TimelineSelector's
// "Diff against parent" button. Returns null when the hash is a session/step
// hash or otherwise doesn't carry diff IDs.
export function parseDiffHash(hash: string): { leftId: string; rightId: string } | null {
  const parts = hash.replace(/^#\/?/, '').split('/')
  if (parts[0] !== 'diff') return null
  const leftRaw = parts[2]
  const rightRaw = parts[3]
  if (!leftRaw || !rightRaw) return null
  // Mirror TimelineSelector's encodeURIComponent on write.
  try {
    return { leftId: decodeURIComponent(leftRaw), rightId: decodeURIComponent(rightRaw) }
  } catch {
    return null
  }
}

export function DiffView({ sessionId }: { sessionId: string }) {
  const { setView } = useStore()
  const [leftId, setLeftId] = useState<string>('')
  const [rightId, setRightId] = useState<string>('')
  const [selectedDiffStep, setSelectedDiffStep] = useState<number | null>(null)

  const { data: timelines = [] } = useQuery({
    queryKey: ['timelines', sessionId],
    queryFn: () => api.sessionTimelines(sessionId),
  })

  const canDiff = !!(leftId && rightId && leftId !== rightId)

  const { data: diff, isLoading: diffLoading } = useQuery({
    queryKey: ['diff', sessionId, leftId, rightId],
    queryFn: () => api.diffTimelines(sessionId, leftId, rightId),
    enabled: canDiff,
  })

  // Reads the hash once on mount (guarded by `!leftId`). We intentionally do
  // NOT register a `hashchange` listener — App.tsx unmounts DiffView when the
  // user navigates away, so each arrival gets a fresh mount and re-reads the
  // hash. If a future routing change keeps DiffView mounted across view
  // transitions, add a `hashchange` listener here to react to URL updates.
  useEffect(() => {
    if (timelines.length >= 2 && !leftId) {
      // URL hash takes precedence — lets TimelineSelector pre-select
      // `left=parent, right=active` for a one-click "Diff against parent",
      // and keeps diff URLs shareable/bookmarkable.
      const fromHash = parseDiffHash(window.location.hash)
      if (fromHash
          && timelines.some(t => t.id === fromHash.leftId)
          && timelines.some(t => t.id === fromHash.rightId)) {
        setLeftId(fromHash.leftId)
        setRightId(fromHash.rightId)
        return
      }
      // Fallback: root vs. first fork.
      const root = timelines.find(t => !t.parent_timeline_id)
      const fork = timelines.find(t => t.parent_timeline_id)
      if (root) setLeftId(root.id)
      if (fork) setRightId(fork.id)
    }
  }, [timelines, leftId])

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-neutral-800 px-4 py-3 flex items-center gap-3">
        <button onClick={() => setView('sessions')} className="text-neutral-400 hover:text-neutral-200">
          <ArrowLeft size={16} />
        </button>
        <h2 className="text-sm font-semibold text-neutral-200">Timeline Diff</h2>

        <div className="flex items-center gap-2 ml-auto">
          <TimelineSelect value={leftId} onChange={setLeftId} timelines={timelines} label="Left" />
          <ArrowLeftRight size={14} className="text-neutral-600" />
          <TimelineSelect value={rightId} onChange={setRightId} timelines={timelines} label="Right" />
        </div>
      </div>

      {diffLoading && <div className="flex-1 flex items-center justify-center text-neutral-500 text-sm">Computing diff...</div>}

      {diff && (
        <div className="flex-1 overflow-auto scrollbar-thin">
          {/* Visual Timeline Diff */}
          <DiffTimeline diff={diff} selectedStep={selectedDiffStep} onSelectStep={setSelectedDiffStep} />

          {diff.diverge_at_step && (
            <div className="px-4 py-2 bg-amber-950/20 border-b border-amber-900/30 text-xs text-amber-400">
              Diverges at step {diff.diverge_at_step}
            </div>
          )}
          <div className="divide-y divide-neutral-800/50">
            {diff.step_diffs.map((entry) => (
              <DiffRow
                key={entry.step_number}
                entry={entry}
                diff={diff}
                isSelected={selectedDiffStep === entry.step_number}
                onSelect={() => setSelectedDiffStep(selectedDiffStep === entry.step_number ? null : entry.step_number)}
              />
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

function TimelineSelect({ value, onChange, timelines, label }: { value: string; onChange: (id: string) => void; timelines: Timeline[]; label: string }) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="bg-neutral-900 border border-neutral-700 rounded px-2 py-1 text-xs text-neutral-300 focus:outline-none focus:border-cyan-700"
    >
      <option value="">{label}</option>
      {timelines.map(t => (
        <option key={t.id} value={t.id}>{t.label} ({t.id.slice(0, 8)})</option>
      ))}
    </select>
  )
}

function DiffRow({ entry, diff, isSelected, onSelect }: { entry: StepDiffEntry; diff: TimelineDiff; isSelected?: boolean; onSelect?: () => void }) {
  const typeStyles: Record<string, string> = {
    Same: 'border-l-green-800',
    Modified: 'border-l-amber-500',
    LeftOnly: 'border-l-red-500',
    RightOnly: 'border-l-blue-500',
  }

  const typeLabel: Record<string, string> = {
    Same: 'Same',
    Modified: 'Modified',
    LeftOnly: `${diff.left_label} only`,
    RightOnly: `${diff.right_label} only`,
  }

  return (
    <div
      onClick={onSelect}
      className={cn(
        'flex items-start border-l-2 px-4 py-2.5 cursor-pointer transition-colors',
        typeStyles[entry.diff_type],
        isSelected ? 'bg-neutral-800/50' : 'hover:bg-neutral-900/50',
      )}
    >
      <div className="w-12 text-xs font-mono text-neutral-500 shrink-0">#{entry.step_number}</div>
      <div className="flex-1 grid grid-cols-2 gap-4">
        {entry.left ? (
          <StepSummaryCell summary={entry.left} />
        ) : (
          <div className="text-xs text-neutral-600 italic">-</div>
        )}
        {entry.right ? (
          <StepSummaryCell summary={entry.right} />
        ) : (
          <div className="text-xs text-neutral-600 italic">-</div>
        )}
      </div>
      <div className="w-20 text-right">
        <span className={cn('text-[10px] font-medium', {
          'text-green-500': entry.diff_type === 'Same',
          'text-amber-400': entry.diff_type === 'Modified',
          'text-red-400': entry.diff_type === 'LeftOnly',
          'text-blue-400': entry.diff_type === 'RightOnly',
        })}>
          {typeLabel[entry.diff_type]}
        </span>
      </div>
    </div>
  )
}

function StepSummaryCell({ summary }: { summary: { step_type: string; status: string; model: string; tokens_in: number; tokens_out: number; duration_ms: number; response_preview: string } }) {
  return (
    <div className="text-xs space-y-0.5">
      <div className="flex items-center gap-2">
        <span className="text-neutral-300 font-medium">{summary.step_type}</span>
        <span className="text-neutral-600 font-mono">{summary.model}</span>
      </div>
      <div className="text-neutral-500">
        {formatDuration(summary.duration_ms)} · {formatTokens(summary.tokens_in + summary.tokens_out)} tokens
      </div>
      {summary.response_preview && (
        <p className="text-neutral-600 truncate">{summary.response_preview}</p>
      )}
    </div>
  )
}

const DIFF_COLORS: Record<string, string> = {
  Same: 'bg-neutral-600',
  Modified: 'bg-amber-500',
  LeftOnly: 'bg-red-500',
  RightOnly: 'bg-green-500',
}

function DiffTimeline({ diff, selectedStep, onSelectStep }: { diff: TimelineDiff; selectedStep: number | null; onSelectStep: (n: number | null) => void }) {
  const total = diff.step_diffs.length
  if (total === 0) return null

  const maxDuration = Math.max(1, ...diff.step_diffs.map(d => {
    const l = d.left?.duration_ms ?? 0
    const r = d.right?.duration_ms ?? 0
    return Math.max(l, r)
  }))

  return (
    <div className="border-b border-neutral-800 bg-neutral-950/50 px-4 py-2">
      <div className="flex items-center gap-2 mb-1.5">
        <span className="text-[10px] uppercase tracking-wider font-semibold text-neutral-500">Timeline Diff</span>
        <div className="flex items-center gap-3 ml-auto text-[9px]">
          <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-sm bg-neutral-600" /> Same</span>
          <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-sm bg-amber-500" /> Modified</span>
          <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-sm bg-red-500" /> {diff.left_label} only</span>
          <span className="flex items-center gap-1"><span className="w-2 h-2 rounded-sm bg-green-500" /> {diff.right_label} only</span>
        </div>
      </div>

      {/* Left timeline */}
      <div className="flex items-center gap-1 mb-1">
        <span className="text-[9px] text-neutral-500 w-12 shrink-0 truncate">{diff.left_label}</span>
        <div className="flex-1 flex gap-px h-5">
          {diff.step_diffs.map((entry) => {
            const width = entry.left ? Math.max(0.5, (entry.left.duration_ms / maxDuration) * 8 + 0.5) : 0.5
            return (
              <button
                key={entry.step_number}
                onClick={() => onSelectStep(selectedStep === entry.step_number ? null : entry.step_number)}
                className={cn(
                  'rounded-sm transition-all flex-shrink-0',
                  DIFF_COLORS[entry.diff_type],
                  entry.diff_type === 'RightOnly' ? 'opacity-20' : 'opacity-70',
                  selectedStep === entry.step_number && 'ring-1 ring-cyan-400 opacity-100',
                )}
                style={{ width: `${width}%`, minWidth: 3 }}
                title={`Step ${entry.step_number}: ${entry.diff_type}`}
              />
            )
          })}
        </div>
      </div>

      {/* Right timeline */}
      <div className="flex items-center gap-1">
        <span className="text-[9px] text-neutral-500 w-12 shrink-0 truncate">{diff.right_label}</span>
        <div className="flex-1 flex gap-px h-5">
          {diff.step_diffs.map((entry) => {
            const width = entry.right ? Math.max(0.5, (entry.right.duration_ms / maxDuration) * 8 + 0.5) : 0.5
            return (
              <button
                key={entry.step_number}
                onClick={() => onSelectStep(selectedStep === entry.step_number ? null : entry.step_number)}
                className={cn(
                  'rounded-sm transition-all flex-shrink-0',
                  DIFF_COLORS[entry.diff_type],
                  entry.diff_type === 'LeftOnly' ? 'opacity-20' : 'opacity-70',
                  selectedStep === entry.step_number && 'ring-1 ring-cyan-400 opacity-100',
                )}
                style={{ width: `${width}%`, minWidth: 3 }}
                title={`Step ${entry.step_number}: ${entry.diff_type}`}
              />
            )
          })}
        </div>
      </div>

      {/* Divergence marker */}
      {diff.diverge_at_step && (
        <div className="flex items-center gap-1 mt-1">
          <span className="text-[9px] text-neutral-500 w-12 shrink-0" />
          <div className="flex-1 relative h-3">
            <div
              className="absolute top-0 h-full border-l border-dashed border-amber-500/60"
              style={{ left: `${((diff.diverge_at_step - 1) / total) * 100}%` }}
            />
            <span
              className="absolute text-[8px] text-amber-500/60 -translate-x-1/2"
              style={{ left: `${((diff.diverge_at_step - 1) / total) * 100}%`, top: 0 }}
            >
              ↑ diverge
            </span>
          </div>
        </div>
      )}
    </div>
  )
}
