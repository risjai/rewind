import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import { useState } from 'react'
import { MessageSquare, FileJson, FileOutput, AlertTriangle, GitBranch, Play, Rocket } from 'lucide-react'
import { JsonTree } from './JsonTree'
import { ForkModal } from './ForkModal'
import { ReplaySetupModal } from './ReplaySetupModal'
import { ReplayJobModal } from './RunReplayButton'

type Tab = 'context' | 'request' | 'response'
type ModalMode = 'fork' | 'replay' | 'runReplay' | null

export function StepDetailPanel({ stepId }: { stepId: string }) {
  const [tab, setTab] = useState<Tab | null>(null)
  const [modalMode, setModalMode] = useState<ModalMode>(null)

  const { data: step, isLoading } = useQuery({
    queryKey: ['step-detail', stepId],
    queryFn: () => api.stepDetail(stepId),
  })

  if (isLoading) {
    return <div className="flex items-center justify-center h-full text-neutral-500 text-sm">Loading step...</div>
  }
  if (!step) {
    return <div className="flex items-center justify-center h-full text-neutral-500 text-sm">Step not found</div>
  }

  // Default tab: 'context' for LLM calls (have messages), 'request' for hook steps
  const isHookStep = step.step_type === 'user_prompt' || step.step_type === 'hook_event' || (!step.messages && step.request_body)
  const activeTab = tab ?? (isHookStep ? 'request' : 'context')

  return (
    <div className="flex flex-col h-full">
      {/* Step info header */}
      <div className="px-4 py-3 border-b border-neutral-800 space-y-2">
        <div className="flex items-center gap-3">
          <span className="text-sm font-mono text-neutral-500">Step #{step.step_number}</span>
          <span className="text-sm font-medium text-neutral-200">{step.step_type}</span>
          {step.tool_name && (
            <span className="text-xs bg-violet-950/50 text-violet-300 px-1.5 py-0.5 rounded border border-violet-800/50 font-mono">{step.tool_name}</span>
          )}
          {step.model && <span className="text-xs bg-neutral-800 text-neutral-400 px-1.5 py-0.5 rounded font-mono">{step.model}</span>}
          <StatusPill status={step.status} />
          <div className="ml-auto flex items-center gap-1.5">
            <button
              onClick={() => setModalMode('fork')}
              title={`Fork a new timeline inheriting steps 1–${step.step_number}`}
              className="flex items-center gap-1 text-[11px] text-amber-400 hover:text-amber-300 border border-amber-900/50 hover:border-amber-700 bg-amber-950/20 hover:bg-amber-950/40 px-2 py-0.5 rounded-md transition-colors"
            >
              <GitBranch size={11} /> Fork from here
            </button>
            <button
              onClick={() => setModalMode('replay')}
              title={`Set up a replay from step ${step.step_number}: cached replay for steps 1–${step.step_number}, live upstream after`}
              className="flex items-center gap-1 text-[11px] text-cyan-400 hover:text-cyan-300 border border-cyan-900/50 hover:border-cyan-700 bg-cyan-950/20 hover:bg-cyan-950/40 px-2 py-0.5 rounded-md transition-colors"
            >
              <Play size={11} /> Set up replay…
            </button>
            <button
              onClick={() => setModalMode('runReplay')}
              title={`Dispatch this replay to a registered runner (e.g. ray-agent). Re-executes the agent against the recorded LLM cache; new steps land on a fresh fork timeline.`}
              className="flex items-center gap-1 text-[11px] text-emerald-400 hover:text-emerald-300 border border-emerald-900/50 hover:border-emerald-700 bg-emerald-950/20 hover:bg-emerald-950/40 px-2 py-0.5 rounded-md transition-colors"
            >
              <Rocket size={11} /> Run replay
            </button>
          </div>
        </div>
        <div className="flex items-center gap-4 text-xs text-neutral-500">
          <span>{formatDuration(step.duration_ms)}</span>
          {(step.tokens_in > 0 || step.tokens_out > 0) && (
            <span>{formatTokens(step.tokens_in)} in / {formatTokens(step.tokens_out)} out</span>
          )}
        </div>
        {step.error && (
          <div className="flex items-center gap-1.5 text-xs text-red-400 bg-red-950/30 px-2.5 py-1.5 rounded border border-red-900/50">
            <AlertTriangle size={12} />
            {step.error}
          </div>
        )}
      </div>

      {/* Tabs */}
      <div className="flex border-b border-neutral-800">
        <TabButton icon={MessageSquare} label="Context Window" active={activeTab === 'context'} onClick={() => setTab('context')} />
        <TabButton icon={FileJson} label="Request" active={activeTab === 'request'} onClick={() => setTab('request')} />
        <TabButton icon={FileOutput} label="Response" active={activeTab === 'response'} onClick={() => setTab('response')} />
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-auto scrollbar-thin">
        {activeTab === 'context' && <ContextWindowView messages={step.messages} />}
        {activeTab === 'request' && <JsonView data={step.request_body} label="Request" />}
        {activeTab === 'response' && <JsonView data={step.response_body} label="Response" />}
      </div>

      {modalMode === 'fork' && (
        <ForkModal
          isOpen
          onClose={() => setModalMode(null)}
          sessionId={step.session_id}
          timelineId={step.timeline_id}
          atStep={step.step_number}
        />
      )}
      {modalMode === 'replay' && (
        <ReplaySetupModal
          isOpen
          onClose={() => setModalMode(null)}
          sessionId={step.session_id}
          timelineId={step.timeline_id}
          atStep={step.step_number}
        />
      )}
      {modalMode === 'runReplay' && (
        <ReplayJobModal
          sessionId={step.session_id}
          sourceTimelineId={step.timeline_id}
          atStep={step.step_number}
          onClose={() => setModalMode(null)}
        />
      )}
    </div>
  )
}

function TabButton({ icon: Icon, label, active, onClick }: { icon: React.ComponentType<{ size?: number }>; label: string; active: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        'flex items-center gap-1.5 px-4 py-2.5 text-xs font-medium border-b-2 transition-colors',
        active
          ? 'border-cyan-400 text-cyan-300'
          : 'border-transparent text-neutral-500 hover:text-neutral-300 hover:border-neutral-700'
      )}
    >
      <Icon size={13} />
      {label}
    </button>
  )
}

function StatusPill({ status }: { status: string }) {
  const styles: Record<string, string> = {
    success: 'bg-green-950 text-green-400 border-green-800',
    error: 'bg-red-950 text-red-400 border-red-800',
    pending: 'bg-amber-950 text-amber-400 border-amber-800',
  }
  return (
    <span className={cn('text-[10px] px-1.5 py-0.5 rounded border font-medium', styles[status] || 'bg-neutral-900 text-neutral-500 border-neutral-700')}>
      {status}
    </span>
  )
}

function ContextWindowView({ messages }: { messages: { role: string; content: string }[] | null }) {
  if (!messages || messages.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm p-4">
        No messages extracted. Check the Request tab for the raw payload.
      </div>
    )
  }

  const roleColors: Record<string, string> = {
    system: 'text-fuchsia-400 bg-fuchsia-950/30 border-fuchsia-900/50',
    user: 'text-cyan-400 bg-cyan-950/30 border-cyan-900/50',
    assistant: 'text-green-400 bg-green-950/30 border-green-900/50',
    tool: 'text-amber-400 bg-amber-950/30 border-amber-900/50',
  }

  return (
    <div className="p-4 space-y-3">
      {messages.map((msg, i) => (
        <div key={i} className={cn('rounded-lg border p-3', roleColors[msg.role] || 'bg-neutral-900 border-neutral-800 text-neutral-300')}>
          <div className="text-[10px] font-semibold uppercase tracking-wider mb-1.5 opacity-80">{msg.role}</div>
          <pre className="text-xs whitespace-pre-wrap leading-relaxed font-mono">{msg.content}</pre>
        </div>
      ))}
    </div>
  )
}

function JsonView({ data, label }: { data: unknown | null; label: string }) {
  if (!data) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
        No {label.toLowerCase()} data
      </div>
    )
  }

  return (
    <div className="p-4">
      <JsonTree data={data} />
    </div>
  )
}
