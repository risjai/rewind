import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import { useState } from 'react'
import { MessageSquare, FileJson, FileOutput, AlertTriangle } from 'lucide-react'
import { JsonTree } from './JsonTree'

type Tab = 'context' | 'request' | 'response'

export function StepDetailPanel({ stepId }: { stepId: string }) {
  const [tab, setTab] = useState<Tab>('context')

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
        <TabButton icon={MessageSquare} label="Context Window" active={tab === 'context'} onClick={() => setTab('context')} />
        <TabButton icon={FileJson} label="Request" active={tab === 'request'} onClick={() => setTab('request')} />
        <TabButton icon={FileOutput} label="Response" active={tab === 'response'} onClick={() => setTab('response')} />
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-auto scrollbar-thin">
        {tab === 'context' && <ContextWindowView messages={step.messages} />}
        {tab === 'request' && <JsonView data={step.request_body} label="Request" />}
        {tab === 'response' && <JsonView data={step.response_body} label="Response" />}
      </div>
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
