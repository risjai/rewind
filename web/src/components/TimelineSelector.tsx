import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { GitBranch, GitCommit, ArrowLeftRight } from 'lucide-react'
import type { Timeline } from '@/types/api'

interface TimelineSelectorProps {
  timelines: Timeline[]
}

export function TimelineSelector({ timelines }: TimelineSelectorProps) {
  const { selectedTimelineId, selectTimeline, setView } = useStore()
  const root = timelines.find(t => !t.parent_timeline_id)
  const activeId = selectedTimelineId || root?.id
  const activeTimeline = timelines.find(t => t.id === activeId)
  const hasParent = !!activeTimeline?.parent_timeline_id

  // Opens the diff view with `left=parent, right=active` pre-selected via
  // URL hash, so the resulting URL is bookmarkable/shareable. The hash is
  // the source of truth; DiffView reads it on mount. (See plan line 139.)
  //
  // Uses `history.replaceState` rather than `window.location.hash = …` so
  // clicking the Diff button doesn't add an entry to browser history. Without
  // this, every click would push a new back-stack entry and Back would bounce
  // the user through hash-only states instead of back to the session view.
  const openDiffAgainstParent = () => {
    if (!activeTimeline?.parent_timeline_id) return
    const sessionId = activeTimeline.session_id
    // UUIDs are URL-safe today, but encodeURIComponent guards against a
    // future backend change that uses richer label formats for timeline IDs.
    const hash = `#/diff/${encodeURIComponent(sessionId)}/${encodeURIComponent(activeTimeline.parent_timeline_id)}/${encodeURIComponent(activeTimeline.id)}`
    window.history.replaceState(null, '', hash)
    setView('diff')
  }

  return (
    <div className="flex items-center gap-2 px-4 py-2 border-b border-neutral-800 bg-neutral-950/50">
      <GitBranch size={14} className="text-neutral-500 shrink-0" />
      {/* Scrollable list of timeline pills. The Diff button stays outside this
          scroller so it's always visible even when there are many timelines. */}
      <div className="flex items-center gap-2 flex-1 min-w-0 overflow-x-auto scrollbar-thin">
        {timelines.map((t) => {
          const isActive = t.id === activeId

          return (
            <button
              key={t.id}
              onClick={() => selectTimeline(t.id)}
              className={cn(
                'flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs font-medium transition-colors shrink-0',
                isActive
                  ? 'bg-neutral-800 text-neutral-100 border border-neutral-700'
                  : 'text-neutral-500 hover:text-neutral-300 hover:bg-neutral-900 border border-transparent'
              )}
            >
              <GitCommit size={12} />
              <span>{t.label}</span>
              {t.fork_at_step && (
                <span className="text-[10px] text-neutral-600">@{t.fork_at_step}</span>
              )}
            </button>
          )
        })}
      </div>
      {hasParent && (
        <button
          onClick={openDiffAgainstParent}
          className="flex items-center gap-1 px-2 py-1 rounded-md text-[11px] text-amber-400 hover:text-amber-300 border border-amber-900/50 hover:border-amber-700 bg-amber-950/20 hover:bg-amber-950/40 transition-colors shrink-0"
          title="Diff this fork against its parent timeline"
        >
          <ArrowLeftRight size={11} /> Diff against parent
        </button>
      )}
    </div>
  )
}
