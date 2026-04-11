import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { useWebSocket } from '@/hooks/use-websocket'
import { StepTimeline } from './StepTimeline'
import { StepDetailPanel } from './StepDetailPanel'
import { TimelineSelector } from './TimelineSelector'
import { SpanTree } from './SpanTree'
import { formatTokens, formatDuration, cn } from '@/lib/utils'
import { Radio, Clock, Layers, Zap, GitBranch, Bot } from 'lucide-react'
import { useCallback, useState } from 'react'
import type { StepResponse } from '@/types/api'

export function SessionView({ sessionId }: { sessionId: string }) {
  const { selectedStepId, selectStep, selectedTimelineId, setView } = useStore()
  const queryClient = useQueryClient()
  const [autoFollow, setAutoFollow] = useState(true)

  const { data: detail, isLoading: detailLoading } = useQuery({
    queryKey: ['session', sessionId],
    queryFn: () => api.session(sessionId),
  })

  const timelineId = selectedTimelineId || detail?.timelines.find(t => !t.parent_timeline_id)?.id
  const { data: steps = [], isLoading: stepsLoading } = useQuery({
    queryKey: ['steps', sessionId, timelineId],
    queryFn: () => api.sessionSteps(sessionId, timelineId),
    enabled: !!timelineId,
  })

  const { data: spans = [] } = useQuery({
    queryKey: ['spans', sessionId, timelineId],
    queryFn: () => api.sessionSpans(sessionId, timelineId),
    enabled: !!timelineId,
  })

  const session = detail?.session
  const isLive = session?.status === 'Recording'

  const onStep = useCallback((step: StepResponse) => {
    queryClient.setQueryData<StepResponse[]>(['steps', sessionId, timelineId], (old) =>
      old ? [...old, step] : [step]
    )
  }, [queryClient, sessionId, timelineId])

  const onSessionUpdate = useCallback((data: { session_id: string; status: string; total_steps: number; total_tokens: number }) => {
    queryClient.invalidateQueries({ queryKey: ['session', sessionId] })
    queryClient.invalidateQueries({ queryKey: ['sessions'] })
  }, [queryClient, sessionId])

  const { connected } = useWebSocket({
    sessionId: isLive ? sessionId : null,
    onStep,
    onSessionUpdate,
  })

  if (detailLoading) {
    return <div className="flex items-center justify-center h-full text-neutral-500">Loading session...</div>
  }
  if (!session) {
    return <div className="flex items-center justify-center h-full text-neutral-500">Session not found</div>
  }

  const totalDuration = steps.reduce((sum, s) => sum + s.duration_ms, 0)
  const hasForked = (detail?.timelines.length ?? 0) > 1

  return (
    <div className="flex flex-col h-full">
      {/* Live banner */}
      {isLive && (
        <div className="bg-cyan-950/30 border-b border-cyan-900/50 px-4 py-2 flex items-center gap-2">
          <Radio size={14} className="text-cyan-400 animate-pulse-dot" />
          <span className="text-xs font-medium text-cyan-300">Recording in progress</span>
          {connected && <span className="text-[10px] text-green-500 ml-auto">WS connected</span>}
          {!connected && <span className="text-[10px] text-red-500 ml-auto">WS disconnected</span>}
        </div>
      )}

      {/* Stats bar */}
      <div className="border-b border-neutral-800 px-4 py-3">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <StatusBadge status={session.status} />
            <h2 className="text-base font-semibold text-neutral-200">{session.name}</h2>
          </div>
          <div className="flex items-center gap-4 text-xs text-neutral-400">
            <span className="flex items-center gap-1"><Layers size={12} /> {session.total_steps} steps</span>
            <span className="flex items-center gap-1"><Zap size={12} /> {formatTokens(session.total_tokens)} tokens</span>
            <span className="flex items-center gap-1"><Clock size={12} /> {formatDuration(totalDuration)}</span>
            {spans.length > 0 && (() => {
              const agentNames = spans.filter(s => s.span_type === 'agent').map(s => s.name);
              return agentNames.length > 0 ? (
                <span className="flex items-center gap-1 text-cyan-400">
                  <Bot size={12} /> {agentNames.join(', ')}
                </span>
              ) : null;
            })()}
            {hasForked && (
              <button
                onClick={() => setView('diff')}
                className="flex items-center gap-1 text-amber-400 hover:text-amber-300 transition-colors"
              >
                <GitBranch size={12} /> {detail!.timelines.length} timelines
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Timeline selector */}
      {hasForked && detail && <TimelineSelector timelines={detail.timelines} />}

      {/* Step timeline + detail */}
      <div className="flex flex-1 overflow-hidden">
        <div className="w-[420px] border-r border-neutral-800 overflow-hidden flex flex-col">
          {stepsLoading ? (
            <div className="flex-1 flex items-center justify-center text-neutral-500 text-sm">Loading steps...</div>
          ) : spans.length > 0 ? (
            <SpanTree
              spans={spans}
              selectedStepId={selectedStepId}
              onSelectStep={selectStep}
            />
          ) : (
            <StepTimeline
              steps={steps}
              selectedStepId={selectedStepId}
              onSelectStep={selectStep}
              autoFollow={autoFollow && isLive}
            />
          )}
        </div>
        <div className="flex-1 overflow-hidden">
          {selectedStepId ? (
            <StepDetailPanel stepId={selectedStepId} />
          ) : (
            <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
              Select a step to inspect
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

function StatusBadge({ status }: { status: string }) {
  const styles: Record<string, string> = {
    Recording: 'bg-cyan-950 text-cyan-300 border-cyan-800',
    Completed: 'bg-green-950 text-green-300 border-green-800',
    Failed: 'bg-red-950 text-red-300 border-red-800',
    Forked: 'bg-amber-950 text-amber-300 border-amber-800',
  }
  return (
    <span className={cn(
      'text-[10px] uppercase tracking-wide font-semibold px-1.5 py-0.5 rounded border',
      styles[status] || 'bg-neutral-900 text-neutral-400 border-neutral-700'
    )}>
      {status}
    </span>
  )
}
