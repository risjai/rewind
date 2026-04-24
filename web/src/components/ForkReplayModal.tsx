import { useState, useEffect, useCallback, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { X, GitBranch, AlertCircle, Loader2, Play, Copy, CheckCircle2 } from 'lucide-react'

type Mode = 'fork' | 'replay'
type Status = 'idle' | 'submitting' | 'error'
type Phase = 'input' | 'instructions'

interface Props {
  isOpen: boolean
  onClose: () => void
  mode: Mode
  sessionId: string
  timelineId?: string
  atStep: number | null
}

// Labels appear in shell commands we hand to the user to paste into their
// terminal. Restrict to a conservative charset so the rendered command can't
// be hijacked by shell metacharacters (`;`, backticks, `$()`, spaces, etc.)
// even if the React-escaped <code> display is safe. Matches the CLI's own
// tolerance for filesystem-safe identifiers.
const LABEL_REGEX = /^[A-Za-z0-9._-]+$/
const LABEL_HINT = 'Use letters, numbers, dot, dash, underscore only.'

export function ForkReplayModal({ isOpen, onClose, mode, sessionId, timelineId, atStep }: Props) {
  const queryClient = useQueryClient()
  const selectTimeline = useStore((s) => s.selectTimeline)
  const defaultLabel = atStep == null ? '' : (mode === 'fork' ? `fork-at-${atStep}` : `replay-from-${atStep}`)
  const [label, setLabel] = useState(defaultLabel)
  const [status, setStatus] = useState<Status>('idle')
  const [phase, setPhase] = useState<Phase>('input')
  const [error, setError] = useState('')
  const [forkedTimelineId, setForkedTimelineId] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  const copiedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const doneBtnRef = useRef<HTMLButtonElement | null>(null)

  useEffect(() => {
    if (isOpen && atStep != null) {
      setLabel(defaultLabel)
      setStatus('idle')
      setPhase('input')
      setError('')
      setForkedTimelineId(null)
      setCopied(false)
      if (copiedTimerRef.current) {
        clearTimeout(copiedTimerRef.current)
        copiedTimerRef.current = null
      }
    }
  }, [isOpen, atStep, mode, defaultLabel])

  // Clear any pending Copied-flag timer on unmount so we never call setState
  // on a disposed component. See santa-review Important-1.
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
    // For replay mode: if a fork was created, navigate to it on close so the
    // user can watch their replay land. Fork mode already does this in handleSubmit.
    if (mode === 'replay' && forkedTimelineId) {
      selectTimeline(forkedTimelineId)
    }
    onClose()
  }, [mode, forkedTimelineId, selectTimeline, onClose])

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
  const replayCommand = `rewind replay ${shortSessionId} --from ${atStep} --label ${effectiveLabel}`

  const handleSubmit = async () => {
    if (!labelIsValid || status === 'submitting') return
    setStatus('submitting')
    setError('')
    try {
      const res = await api.forkSession(sessionId, {
        at_step: atStep,
        label: effectiveLabel,
        timeline_id: timelineId,
      })
      await queryClient.invalidateQueries({ queryKey: ['session', sessionId] })
      await queryClient.invalidateQueries({ queryKey: ['timelines', sessionId] })
      setForkedTimelineId(res.fork_timeline_id)
      setStatus('idle')
      if (mode === 'fork') {
        selectTimeline(res.fork_timeline_id)
        onClose()
      } else {
        setPhase('instructions')
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Fork failed'
      setError(msg)
      setStatus('error')
    }
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

  const title = mode === 'fork' ? 'Fork from step' : 'Set up replay from step'
  const PrimaryIcon = mode === 'fork' ? GitBranch : Play
  const primaryLabel = mode === 'fork' ? 'Create fork' : 'Set up replay'
  const primaryLabelPending = mode === 'fork' ? 'Forking…' : 'Creating fork…'
  const submitDisabled = status === 'submitting' || !labelIsValid

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div
        className="absolute inset-0 bg-black/60"
        onClick={() => status !== 'submitting' && close()}
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
            onClick={close}
            disabled={status === 'submitting'}
            className="text-neutral-500 hover:text-neutral-300 transition-colors disabled:opacity-50"
          >
            <X size={16} />
          </button>
        </div>

        {phase === 'input' ? (
          <>
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
                  aria-describedby="fork-replay-label-hint"
                  className={cn(
                    'w-full bg-neutral-800 border rounded-lg px-3 py-1.5 text-xs text-neutral-200 placeholder:text-neutral-500 focus:outline-none focus:ring-1',
                    labelIsValid
                      ? 'border-neutral-700 focus:border-cyan-600 focus:ring-cyan-600'
                      : 'border-red-800 focus:border-red-600 focus:ring-red-600',
                  )}
                />
                <p
                  id="fork-replay-label-hint"
                  className={cn('text-[11px] mt-1.5', labelIsValid ? 'text-neutral-500' : 'text-red-400')}
                >
                  {!labelIsValid
                    ? LABEL_HINT
                    : mode === 'fork'
                      ? `Creates a new timeline that inherits steps 1–${atStep} from this session.`
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
                onClick={close}
                disabled={status === 'submitting'}
                className="px-3 py-1.5 rounded-lg text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200 transition-colors disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                onClick={handleSubmit}
                disabled={submitDisabled}
                className={cn(
                  'flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors',
                  submitDisabled
                    ? 'bg-neutral-700 text-neutral-400 cursor-not-allowed'
                    : 'bg-amber-600 text-white hover:bg-amber-500',
                )}
              >
                {status === 'submitting' ? (
                  <><Loader2 size={12} className="animate-spin" /> {primaryLabelPending}</>
                ) : (
                  <><PrimaryIcon size={12} /> {primaryLabel}</>
                )}
              </button>
            </div>
          </>
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
