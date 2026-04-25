import { useState, useEffect, useCallback, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { X, Trash2, AlertCircle, Loader2, AlertTriangle } from 'lucide-react'
import type { Timeline } from '@/types/api'

type Status = 'idle' | 'submitting' | 'error'

interface Props {
  isOpen: boolean
  onClose: () => void
  sessionId: string
  /// The fork being deleted. Root timelines are not deletable; callers must
  /// not open this modal for them (the TimelineSelector trash icon is hidden
  /// for root).
  timeline: Timeline | null
}

// Destructive — hard-deletes a fork plus its steps, spans, replay contexts,
// scores, and step counters. Server-side invariants (no children, no
// baselines, no active replay) are mapped to a 409 response; we surface
// the message so the user sees *why* the delete was blocked.
export function DeleteTimelineConfirmModal({ isOpen, onClose, sessionId, timeline }: Props) {
  const queryClient = useQueryClient()
  const selectTimeline = useStore((s) => s.selectTimeline)
  const selectedTimelineId = useStore((s) => s.selectedTimelineId)
  const [status, setStatus] = useState<Status>('idle')
  const [error, setError] = useState('')
  const cancelBtnRef = useRef<HTMLButtonElement | null>(null)
  // Element focused before the modal opened — we return focus here on close
  // so keyboard users aren't dropped onto <body>.
  const previousFocusRef = useRef<HTMLElement | null>(null)

  useEffect(() => {
    if (isOpen) {
      setStatus('idle')
      setError('')
      // Capture the active element so we can restore focus on close.
      previousFocusRef.current = document.activeElement as HTMLElement | null
      // Initial focus on Cancel (safer default than the destructive button
      // for a confirm dialog — users who hit Enter don't accidentally delete).
      cancelBtnRef.current?.focus()
    } else if (previousFocusRef.current) {
      previousFocusRef.current.focus()
      previousFocusRef.current = null
    }
  }, [isOpen, timeline?.id])

  const close = useCallback(() => onClose(), [onClose])

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && status !== 'submitting') close()
    }
    if (isOpen) document.addEventListener('keydown', handleKey)
    return () => document.removeEventListener('keydown', handleKey)
  }, [isOpen, close, status])

  if (!isOpen || !timeline) return null

  const handleDelete = async () => {
    if (status === 'submitting') return
    setStatus('submitting')
    setError('')
    try {
      await api.deleteTimeline(sessionId, timeline.id)
      // If the user was viewing the fork they just deleted, send them back
      // to the parent (or null → UI will resolve to root).
      if (selectedTimelineId === timeline.id) {
        selectTimeline(timeline.parent_timeline_id)
      }
      // Drop cached data for the deleted timeline — the data is gone, not
      // stale, so removeQueries (not invalidateQueries). Invalidate the
      // session- and timelines-level caches so other views pick up the
      // deletion on their next fetch.
      queryClient.removeQueries({ queryKey: ['steps', sessionId, timeline.id] })
      queryClient.removeQueries({ queryKey: ['spans', sessionId, timeline.id] })
      await queryClient.invalidateQueries({ queryKey: ['session', sessionId] })
      await queryClient.invalidateQueries({ queryKey: ['timelines', sessionId] })
      onClose()
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Delete failed'
      setError(msg)
      setStatus('error')
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/60"
        onClick={() => status !== 'submitting' && close()}
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Delete fork"
        className="relative bg-neutral-900 border border-neutral-700 rounded-xl shadow-2xl w-full max-w-md mx-4"
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-neutral-800">
          <div className="flex items-center gap-2">
            <AlertTriangle size={16} className="text-red-400" />
            <h3 className="text-sm font-semibold text-neutral-200">Delete fork</h3>
          </div>
          <button
            onClick={close}
            disabled={status === 'submitting'}
            className="text-neutral-500 hover:text-neutral-300 transition-colors disabled:opacity-50"
          >
            <X size={16} />
          </button>
        </div>

        <div className="px-5 py-4 space-y-3">
          <p className="text-xs text-neutral-300">
            Permanently delete fork{' '}
            <span className="font-mono text-neutral-100">{timeline.label}</span>
            {timeline.fork_at_step != null && (
              <span className="text-neutral-500"> (from step {timeline.fork_at_step})</span>
            )}?
          </p>
          <p className="text-[11px] text-neutral-500">
            This removes the fork plus every step, span, replay context, and score attached to it. This cannot be undone.
          </p>

          {status === 'error' && error && (
            <div className="flex items-start gap-2 bg-red-950/30 border border-red-900/50 rounded-lg px-3 py-2.5">
              <AlertCircle size={14} className="text-red-400 mt-0.5 shrink-0" />
              <p className="text-xs text-red-300 break-all">{error}</p>
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 px-5 py-3 border-t border-neutral-800">
          <button
            ref={cancelBtnRef}
            onClick={close}
            disabled={status === 'submitting'}
            className="px-3 py-1.5 rounded-lg text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200 transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={handleDelete}
            disabled={status === 'submitting'}
            className={cn(
              'flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors',
              status === 'submitting'
                ? 'bg-neutral-700 text-neutral-400 cursor-not-allowed'
                : 'bg-red-600 text-white hover:bg-red-500',
            )}
          >
            {status === 'submitting' ? (
              <><Loader2 size={12} className="animate-spin" /> Deleting…</>
            ) : (
              <><Trash2 size={12} /> Delete fork</>
            )}
          </button>
        </div>
      </div>
    </div>
  )
}
