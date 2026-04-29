import { useState, useCallback } from 'react'
import { useQueryClient, useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'

export function useStepEdit(params: {
  stepId: string
  sessionId: string
  timelineId: string
}) {
  const { stepId, sessionId, timelineId } = params
  const queryClient = useQueryClient()

  const [confirmOpen, setConfirmOpen] = useState(false)
  const [isMutating, setIsMutating] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [autoForked, setAutoForked] = useState(false)
  const [forkTimelineId, setForkTimelineId] = useState<string | null>(null)
  const [pendingPayload, setPendingPayload] = useState<{
    editField: 'request' | 'response'
    text: string
  } | null>(null)

  const { data: health } = useQuery({
    queryKey: ['health'],
    queryFn: () => api.health(),
    staleTime: 60_000,
  })

  const { data: cascade, refetch: refetchCascade } = useQuery({
    queryKey: ['cascade-count', stepId],
    queryFn: () => api.cascadeCount(stepId),
    enabled: false,
  })

  const { data: sessionDetail } = useQuery({
    queryKey: ['session', sessionId],
    queryFn: () => api.session(sessionId),
  })

  const allowMainEdits = health?.allow_main_edits ?? false
  const onMain = (() => {
    if (!sessionDetail) return false
    const tl = sessionDetail.timelines.find((t) => t.id === timelineId)
    return tl ? tl.parent_timeline_id === null : false
  })()

  const cascadeCount = cascade?.deleted_downstream_count ?? 0

  const openConfirm = useCallback(
    async (editField: 'request' | 'response', text: string) => {
      setPendingPayload({ editField, text })
      setError(null)
      await refetchCascade()
      setConfirmOpen(true)
    },
    [refetchCascade],
  )

  const cancelConfirm = useCallback(() => {
    setConfirmOpen(false)
    setPendingPayload(null)
  }, [])

  const save = useCallback(async (): Promise<{
    deleted_downstream_count: number
  } | null> => {
    if (!pendingPayload) return null
    setIsMutating(true)
    setError(null)

    try {
      const canonical = JSON.stringify(JSON.parse(pendingPayload.text))
      const bodyKey =
        pendingPayload.editField === 'request' ? 'request_body' : 'response_body'
      const payload = { [bodyKey]: JSON.parse(canonical) }

      let result: { deleted_downstream_count: number }

      if (!onMain || allowMainEdits) {
        const res = await api.patchStep(stepId, payload)
        result = { deleted_downstream_count: res.deleted_downstream_count }
      } else {
        const step = await api.stepDetail(stepId)
        const res = await api.forkAndEditStep(sessionId, {
          source_timeline_id: timelineId,
          at_step: step.step_number,
          ...payload,
        })
        setAutoForked(true)
        setForkTimelineId(res.fork_timeline_id)
        result = { deleted_downstream_count: 0 }
      }

      await queryClient.invalidateQueries({ queryKey: ['step-detail', stepId] })
      await queryClient.invalidateQueries({ queryKey: ['session', sessionId] })
      await queryClient.invalidateQueries({ queryKey: ['timelines', sessionId] })

      setConfirmOpen(false)
      setPendingPayload(null)
      return result
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Save failed')
      return null
    } finally {
      setIsMutating(false)
    }
  }, [
    pendingPayload,
    onMain,
    allowMainEdits,
    stepId,
    sessionId,
    timelineId,
    queryClient,
  ])

  return {
    save,
    isMutating,
    error,
    cascadeCount,
    onMain,
    allowMainEdits,
    confirmOpen,
    openConfirm,
    cancelConfirm,
    autoForked,
    forkTimelineId,
  }
}
