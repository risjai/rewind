import { useState, useEffect } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { X, GitBranch, AlertCircle, Loader2 } from 'lucide-react'

type Mode = 'fork' | 'replay'
type Status = 'idle' | 'submitting' | 'error'

interface Props {
  isOpen: boolean
  onClose: () => void
  mode: Mode
  sessionId: string
  timelineId?: string
  atStep: number | null
}

export function ForkReplayModal({ isOpen, onClose, mode, sessionId, timelineId, atStep }: Props) {
  const queryClient = useQueryClient()
  const selectTimeline = useStore((s) => s.selectTimeline)
  const defaultLabel = atStep == null ? '' : (mode === 'fork' ? `fork-at-${atStep}` : `replay-from-${atStep}`)
  const [label, setLabel] = useState(defaultLabel)
  const [status, setStatus] = useState<Status>('idle')
  const [error, setError] = useState('')

  useEffect(() => {
    if (isOpen && atStep != null) {
      setLabel(defaultLabel)
      setStatus('idle')
      setError('')
    }
  }, [isOpen, atStep, defaultLabel])

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && status !== 'submitting') onClose()
    }
    if (isOpen) document.addEventListener('keydown', handleKey)
    return () => document.removeEventListener('keydown', handleKey)
  }, [isOpen, onClose, status])

  if (!isOpen || atStep == null) return null

  const handleSubmit = async () => {
    setStatus('submitting')
    setError('')
    try {
      const res = await api.forkSession(sessionId, {
        at_step: atStep,
        label: label.trim() || defaultLabel,
        timeline_id: timelineId,
      })
      await queryClient.invalidateQueries({ queryKey: ['session', sessionId] })
      await queryClient.invalidateQueries({ queryKey: ['timelines', sessionId] })
      selectTimeline(res.fork_timeline_id)
      onClose()
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Fork failed'
      setError(msg)
      setStatus('error')
    }
  }

  const title = mode === 'fork' ? 'Fork from step' : 'Set up replay from step'

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/60"
        onClick={() => status !== 'submitting' && onClose()}
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className="relative bg-neutral-900 border border-neutral-700 rounded-xl shadow-2xl w-full max-w-md mx-4"
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-neutral-800">
          <div className="flex items-center gap-2">
            <GitBranch size={16} className="text-amber-400" />
            <h3 className="text-sm font-semibold text-neutral-200">{title} #{atStep}</h3>
          </div>
          <button
            onClick={onClose}
            disabled={status === 'submitting'}
            className="text-neutral-500 hover:text-neutral-300 transition-colors disabled:opacity-50"
          >
            <X size={16} />
          </button>
        </div>

        <div className="px-5 py-4 space-y-4">
          <div>
            <label className="block text-xs font-medium text-neutral-400 mb-1.5">Label</label>
            <input
              type="text"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder={defaultLabel}
              autoFocus
              className="w-full bg-neutral-800 border border-neutral-700 rounded-lg px-3 py-1.5 text-xs text-neutral-200 placeholder:text-neutral-500 focus:border-cyan-600 focus:outline-none focus:ring-1 focus:ring-cyan-600"
            />
            <p className="text-[11px] text-neutral-500 mt-1.5">
              Creates a new timeline that inherits steps 1–{atStep} from this session.
            </p>
          </div>

          {status === 'error' && error && (
            <div className="flex items-start gap-2 bg-red-950/30 border border-red-900/50 rounded-lg px-3 py-2.5">
              <AlertCircle size={14} className="text-red-400 mt-0.5 shrink-0" />
              <p className="text-xs text-red-300 break-all">{error}</p>
            </div>
          )}
        </div>

        <div className="flex justify-end gap-2 px-5 py-3 border-t border-neutral-800">
          <button
            onClick={onClose}
            disabled={status === 'submitting'}
            className="px-3 py-1.5 rounded-lg text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200 transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={handleSubmit}
            disabled={status === 'submitting'}
            className={cn(
              'flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors',
              status === 'submitting'
                ? 'bg-neutral-700 text-neutral-400 cursor-not-allowed'
                : 'bg-amber-600 text-white hover:bg-amber-500'
            )}
          >
            {status === 'submitting' ? (
              <><Loader2 size={12} className="animate-spin" /> Forking…</>
            ) : (
              <><GitBranch size={12} /> Create fork</>
            )}
          </button>
        </div>
      </div>
    </div>
  )
}
