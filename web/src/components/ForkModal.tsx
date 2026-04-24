import { useState, useEffect, useCallback } from 'react'
import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { X, GitBranch, AlertCircle, Loader2 } from 'lucide-react'
import { useForkMutation, LABEL_REGEX, LABEL_HINT } from '@/hooks/use-fork-mutation'

interface Props {
  isOpen: boolean
  onClose: () => void
  sessionId: string
  timelineId?: string
  atStep: number | null
}

// Fork a session at a step, then navigate to the new timeline.
// Keeps the existing `ForkReplayModal` fork-mode behavior bit-for-bit — only
// the replay-mode branch moved to `ReplaySetupModal`. See
// `plans/fork-replay-web-ui.md` for why the split was required.
export function ForkModal({ isOpen, onClose, sessionId, timelineId, atStep }: Props) {
  const selectTimeline = useStore((s) => s.selectTimeline)
  const defaultLabel = atStep == null ? '' : `fork-at-${atStep}`
  const [label, setLabel] = useState(defaultLabel)
  const { status, error, submit, reset } = useForkMutation()

  useEffect(() => {
    if (isOpen && atStep != null) {
      setLabel(defaultLabel)
      reset()
    }
  }, [isOpen, atStep, defaultLabel, reset])

  const close = useCallback(() => onClose(), [onClose])

  useEffect(() => {
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && status !== 'submitting') close()
    }
    if (isOpen) document.addEventListener('keydown', handleKey)
    return () => document.removeEventListener('keydown', handleKey)
  }, [isOpen, close, status])

  if (!isOpen || atStep == null) return null

  const effectiveLabel = label.trim() || defaultLabel
  const labelIsValid = LABEL_REGEX.test(effectiveLabel)
  const submitDisabled = status === 'submitting' || !labelIsValid

  const handleSubmit = async () => {
    if (!labelIsValid) return
    const forkId = await submit({ sessionId, atStep, label: effectiveLabel, timelineId })
    if (forkId) {
      selectTimeline(forkId)
      onClose()
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
        aria-label="Fork from step"
        className="relative bg-neutral-900 border border-neutral-700 rounded-xl shadow-2xl w-full max-w-md mx-4"
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-neutral-800">
          <div className="flex items-center gap-2">
            <GitBranch size={16} className="text-amber-400" />
            <h3 className="text-sm font-semibold text-neutral-200">Fork from step #{atStep}</h3>
          </div>
          <button
            onClick={close}
            disabled={status === 'submitting'}
            className="text-neutral-500 hover:text-neutral-300 transition-colors disabled:opacity-50"
          >
            <X size={16} />
          </button>
        </div>

        <form onSubmit={(e) => { e.preventDefault(); handleSubmit() }}>
          <div className="px-5 py-4 space-y-4">
            <div>
              <label className="block text-xs font-medium text-neutral-400 mb-1.5">Label</label>
              <input
                type="text"
                value={label}
                onChange={(e) => setLabel(e.target.value)}
                placeholder={defaultLabel}
                autoFocus
                aria-invalid={!labelIsValid}
                aria-describedby="fork-label-hint"
                className={cn(
                  'w-full bg-neutral-800 border rounded-lg px-3 py-1.5 text-xs text-neutral-200 placeholder:text-neutral-500 focus:outline-none focus:ring-1',
                  labelIsValid
                    ? 'border-neutral-700 focus:border-cyan-600 focus:ring-cyan-600'
                    : 'border-red-800 focus:border-red-600 focus:ring-red-600',
                )}
              />
              <p
                id="fork-label-hint"
                className={cn('text-[11px] mt-1.5', labelIsValid ? 'text-neutral-500' : 'text-red-400')}
              >
                {!labelIsValid
                  ? LABEL_HINT
                  : `Creates a new timeline that inherits steps 1–${atStep} from this session.`}
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
              type="button"
              onClick={close}
              disabled={status === 'submitting'}
              className="px-3 py-1.5 rounded-lg text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200 transition-colors disabled:opacity-50"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={submitDisabled}
              className={cn(
                'flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors',
                submitDisabled
                  ? 'bg-neutral-700 text-neutral-400 cursor-not-allowed'
                  : 'bg-amber-600 text-white hover:bg-amber-500',
              )}
            >
              {status === 'submitting' ? (
                <><Loader2 size={12} className="animate-spin" /> Forking…</>
              ) : (
                <><GitBranch size={12} /> Create fork</>
              )}
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}
