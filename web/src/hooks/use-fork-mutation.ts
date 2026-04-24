import { useState, useCallback } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'

export type ForkMutationStatus = 'idle' | 'submitting' | 'error'

interface SubmitParams {
  sessionId: string
  atStep: number
  label: string
  timelineId?: string
}

// Shared mutation state + submit handler for the Fork and ReplaySetup modals.
// Owns only the API + query-cache concerns — callers handle navigation, phase
// transitions, and UI wiring. Extracted when `ForkReplayModal` tripped the
// 150-LOC tripwire in `plans/fork-replay-web-ui.md`.
export function useForkMutation() {
  const queryClient = useQueryClient()
  const [status, setStatus] = useState<ForkMutationStatus>('idle')
  const [error, setError] = useState('')
  const [forkedTimelineId, setForkedTimelineId] = useState<string | null>(null)

  const reset = useCallback(() => {
    setStatus('idle')
    setError('')
    setForkedTimelineId(null)
  }, [])

  const submit = useCallback(async ({ sessionId, atStep, label, timelineId }: SubmitParams) => {
    if (status === 'submitting') return null
    setStatus('submitting')
    setError('')
    try {
      const res = await api.forkSession(sessionId, {
        at_step: atStep,
        label,
        timeline_id: timelineId,
      })
      await queryClient.invalidateQueries({ queryKey: ['session', sessionId] })
      await queryClient.invalidateQueries({ queryKey: ['timelines', sessionId] })
      setForkedTimelineId(res.fork_timeline_id)
      setStatus('idle')
      return res.fork_timeline_id
    } catch (e) {
      const msg = e instanceof Error ? e.message : 'Fork failed'
      setError(msg)
      setStatus('error')
      return null
    }
  }, [queryClient, status])

  return { status, error, forkedTimelineId, submit, reset }
}

// Labels appear in shell commands we hand to the user to paste into their
// terminal. Restrict to a conservative charset so the rendered command can't
// be hijacked by shell metacharacters (`;`, backticks, `$()`, spaces, etc.)
// even if the React-escaped <code> display is safe. Matches the CLI's own
// tolerance for filesystem-safe identifiers.
export const LABEL_REGEX = /^[A-Za-z0-9._-]+$/
export const LABEL_HINT = 'Use letters, numbers, dot, dash, underscore only.'
