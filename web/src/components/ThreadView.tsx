import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { formatTokens, timeAgo, cn } from '@/lib/utils'
import { ArrowLeft, MessageSquare, Layers, Zap } from 'lucide-react'
import type { Session } from '@/types/api'

interface ThreadViewProps {
  threadId: string
  onBack: () => void
}

export function ThreadView({ threadId, onBack }: ThreadViewProps) {
  const { selectSession } = useStore()

  const { data, isLoading } = useQuery({
    queryKey: ['thread', threadId],
    queryFn: () => api.thread(threadId),
  })

  if (isLoading) {
    return <div className="flex items-center justify-center h-full text-neutral-500">Loading thread...</div>
  }

  if (!data || data.sessions.length === 0) {
    return <div className="flex items-center justify-center h-full text-neutral-500">Thread not found</div>
  }

  const totalSteps = data.sessions.reduce((sum, s) => sum + s.total_steps, 0)
  const totalTokens = data.sessions.reduce((sum, s) => sum + s.total_tokens, 0)

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-neutral-800 px-4 py-3">
        <div className="flex items-center gap-3">
          <button onClick={onBack} className="p-1 rounded hover:bg-neutral-800 text-neutral-400 hover:text-neutral-200">
            <ArrowLeft size={18} />
          </button>
          <div>
            <h2 className="text-base font-semibold text-neutral-200 flex items-center gap-2">
              <MessageSquare size={16} className="text-cyan-400" />
              {threadId}
            </h2>
            <div className="flex items-center gap-4 text-xs text-neutral-500 mt-1">
              <span>{data.sessions.length} turns</span>
              <span className="flex items-center gap-1"><Layers size={11} /> {totalSteps} steps</span>
              <span className="flex items-center gap-1"><Zap size={11} /> {formatTokens(totalTokens)} tokens</span>
            </div>
          </div>
        </div>
      </div>

      <div className="flex-1 overflow-auto p-4 space-y-3">
        {data.sessions.map((session, i) => (
          <TurnCard
            key={session.id}
            session={session}
            turnNumber={i + 1}
            onClick={() => selectSession(session.id)}
          />
        ))}
      </div>
    </div>
  )
}

function TurnCard({ session, turnNumber, onClick }: { session: Session; turnNumber: number; onClick: () => void }) {
  const statusColors: Record<string, string> = {
    Recording: 'border-cyan-800 bg-cyan-950/20',
    Completed: 'border-green-900/50 bg-green-950/10',
    Failed: 'border-red-900/50 bg-red-950/10',
    Forked: 'border-amber-900/50 bg-amber-950/10',
  }

  const statusDotColors: Record<string, string> = {
    Recording: 'bg-cyan-400 animate-pulse-dot',
    Completed: 'bg-green-500',
    Failed: 'bg-red-500',
    Forked: 'bg-amber-500',
  }

  return (
    <button
      onClick={onClick}
      className={cn(
        'w-full text-left p-4 rounded-lg border transition-colors hover:bg-neutral-800/30',
        statusColors[session.status] || 'border-neutral-800'
      )}
    >
      <div className="flex items-center gap-3 mb-2">
        <span className="text-xs font-semibold text-neutral-500 bg-neutral-800 px-2 py-0.5 rounded">
          Turn {turnNumber}
        </span>
        <span className={cn('w-2 h-2 rounded-full shrink-0', statusDotColors[session.status] || 'bg-neutral-600')} />
        <span className="text-sm font-medium text-neutral-200">{session.name}</span>
        <span className="text-xs text-neutral-600 ml-auto">{timeAgo(session.created_at)}</span>
      </div>
      <div className="flex items-center gap-4 text-xs text-neutral-500 pl-16">
        <span>{session.total_steps} steps</span>
        <span>{formatTokens(session.total_tokens)} tokens</span>
      </div>
    </button>
  )
}
