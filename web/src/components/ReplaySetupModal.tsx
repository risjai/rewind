import { useState, useEffect, useCallback, useRef } from 'react'
import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { X, GitBranch, AlertCircle, Loader2, Play, Copy, CheckCircle2 } from 'lucide-react'
import { useForkMutation, LABEL_REGEX, LABEL_HINT } from '@/hooks/use-fork-mutation'

type Phase = 'input' | 'instructions'

interface Props {
  isOpen: boolean
  onClose: () => void
  sessionId: string
  timelineId?: string
  atStep: number | null
}

// Set up a replay: create a fork server-side, then show the CLI command the
// user runs in their terminal. The proxy is external — the web UI just helps
// the user produce the right command. See `plans/fork-replay-web-ui.md`.
export function ReplaySetupModal({ isOpen, onClose, sessionId, timelineId, atStep }: Props) {
  const selectTimeline = useStore((s) => s.selectTimeline)
  const defaultLabel = atStep == null ? '' : `replay-from-${atStep}`
  const [label, setLabel] = useState(defaultLabel)
  const [phase, setPhase] = useState<Phase>('input')
  const [copied, setCopied] = useState(false)
  const copiedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const doneBtnRef = useRef<HTMLButtonElement | null>(null)
  const { status, error, forkedTimelineId, submit, reset } = useForkMutation()

  useEffect(() => {
    if (isOpen && atStep != null) {
      setLabel(defaultLabel)
      setPhase('input')
      setCopied(false)
      reset()
      if (copiedTimerRef.current) {
        clearTimeout(copiedTimerRef.current)
        copiedTimerRef.current = null
      }
    }
  }, [isOpen, atStep, defaultLabel, reset])

  // Clear any pending Copied-flag timer on unmount so we never call setState
  // on a disposed component.
  useEffect(() => {
    return () => {
      if (copiedTimerRef.current) {
        clearTimeout(copiedTimerRef.current)
        copiedTimerRef.current = null
      }
    }
  }, [])

  // When the modal flips to the instructions panel, move focus to the Done
  // button so keyboard / screen-reader users aren't dropped onto <body>.
  useEffect(() => {
    if (phase === 'instructions') {
      doneBtnRef.current?.focus()
    }
  }, [phase])

  const close = useCallback(() => {
    // If a fork was created, navigate to it on close so the user can watch
    // their replay land on the fork timeline.
    if (forkedTimelineId) {
      selectTimeline(forkedTimelineId)
    }
    onClose()
  }, [forkedTimelineId, selectTimeline, onClose])

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
  const shortSessionId = sessionId.slice(0, 8)
  // --fork-id pins the CLI to the fork the UI is watching (issue #140).
  // `replayCommand` is only rendered inside the `instructions` phase, which
  // only appears after `forkedTimelineId` is set — so the fallback empty
  // string below is just a non-null placeholder, never displayed.
  const replayCommand = forkedTimelineId
    ? `rewind replay ${shortSessionId} --from ${atStep} --fork-id ${forkedTimelineId}`
    : ''
  const submitDisabled = status === 'submitting' || !labelIsValid

  const handleSubmit = async () => {
    if (!labelIsValid) return
    const forkId = await submit({ sessionId, atStep, label: effectiveLabel, timelineId })
    if (forkId) setPhase('instructions')
  }

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(replayCommand)
      setCopied(true)
      if (copiedTimerRef.current) clearTimeout(copiedTimerRef.current)
      copiedTimerRef.current = setTimeout(() => {
        setCopied(false)
        copiedTimerRef.current = null
      }, 2000)
    } catch {
      // clipboard write denied — leave copied=false, user can still select + Ctrl+C
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
        aria-label="Set up replay from step"
        className="relative bg-neutral-900 border border-neutral-700 rounded-xl shadow-2xl w-full max-w-md mx-4"
      >
        <div className="flex items-center justify-between px-5 py-4 border-b border-neutral-800">
          <div className="flex items-center gap-2">
            <GitBranch size={16} className="text-amber-400" />
            <h3 className="text-sm font-semibold text-neutral-200">Set up replay from step #{atStep}</h3>
          </div>
          <button
            onClick={close}
            disabled={status === 'submitting'}
            className="text-neutral-500 hover:text-neutral-300 transition-colors disabled:opacity-50"
          >
            <X size={16} />
          </button>
        </div>

        {phase === 'input' ? (
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
                  aria-describedby="replay-label-hint"
                  className={cn(
                    'w-full bg-neutral-800 border rounded-lg px-3 py-1.5 text-xs text-neutral-200 placeholder:text-neutral-500 focus:outline-none focus:ring-1',
                    labelIsValid
                      ? 'border-neutral-700 focus:border-cyan-600 focus:ring-cyan-600'
                      : 'border-red-800 focus:border-red-600 focus:ring-red-600',
                  )}
                />
                <p
                  id="replay-label-hint"
                  className={cn('text-[11px] mt-1.5', labelIsValid ? 'text-neutral-500' : 'text-red-400')}
                >
                  {!labelIsValid
                    ? LABEL_HINT
                    : `Creates a fork at step ${atStep}, then shows you the CLI command to start the replay proxy.`}
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
                  <><Loader2 size={12} className="animate-spin" /> Creating fork…</>
                ) : (
                  <><Play size={12} /> Set up replay</>
                )}
              </button>
            </div>
          </form>
        ) : (
          <>
            <div className="px-5 py-4 space-y-4">
              <div
                role="status"
                aria-live="polite"
                className="flex items-start gap-2 bg-green-950/30 border border-green-900/50 rounded-lg px-3 py-2.5"
              >
                <CheckCircle2 size={14} className="text-green-400 mt-0.5 shrink-0" />
                <div className="text-xs">
                  <p className="text-green-300 font-medium">Fork created: {effectiveLabel}</p>
                  <p className="text-green-400/70 mt-0.5">
                    Steps 1–{atStep} replay from cache (0 ms, 0 tokens). Subsequent steps hit the live upstream.
                  </p>
                </div>
              </div>

              <div>
                <label className="block text-xs font-medium text-neutral-400 mb-1.5">Run this in your terminal</label>
                <div className="flex items-stretch gap-1">
                  <code className="flex-1 bg-neutral-950 border border-neutral-800 rounded-lg px-3 py-2 text-[11px] text-neutral-200 font-mono break-all">
                    {replayCommand}
                  </code>
                  <button
                    onClick={handleCopy}
                    aria-label="Copy command"
                    className={cn(
                      'flex items-center gap-1 px-2.5 rounded-lg text-xs border transition-colors',
                      copied
                        ? 'bg-green-950/40 border-green-800 text-green-300'
                        : 'bg-neutral-800 border-neutral-700 text-neutral-300 hover:bg-neutral-700',
                    )}
                  >
                    {copied ? <><CheckCircle2 size={12} /> Copied</> : <><Copy size={12} /> Copy</>}
                  </button>
                </div>
                <p className="text-[11px] text-neutral-500 mt-1.5">
                  Then re-run your agent pointing at the replay proxy (default: http://127.0.0.1:8443).
                  New steps will stream into the fork timeline here.
                </p>
              </div>
            </div>

            <div className="flex justify-end gap-2 px-5 py-3 border-t border-neutral-800">
              <button
                ref={doneBtnRef}
                onClick={close}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium bg-cyan-600 text-white hover:bg-cyan-500 transition-colors"
              >
                Done
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  )
}
