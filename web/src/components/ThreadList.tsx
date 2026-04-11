import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { formatTokens } from '@/lib/utils'
import { MessageSquare } from 'lucide-react'
import { ThreadView } from './ThreadView'
import type { ThreadSummary } from '@/types/api'

export function ThreadList() {
  const { selectedThreadId, selectThread } = useStore()

  const { data: threads = [], isLoading } = useQuery({
    queryKey: ['threads'],
    queryFn: api.threads,
    refetchInterval: 10000,
  })

  if (selectedThreadId) {
    return <ThreadView threadId={selectedThreadId} onBack={() => selectThread(null)} />
  }

  if (isLoading) {
    return <div className="flex items-center justify-center h-full text-neutral-500">Loading threads...</div>
  }

  if (threads.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500">
        <div className="text-center space-y-3">
          <MessageSquare size={40} className="mx-auto text-neutral-600" />
          <h2 className="text-lg font-semibold text-neutral-300">No conversation threads</h2>
          <p className="text-sm max-w-md">
            Threads group related sessions into multi-turn conversations.
            Use <code className="text-cyan-400 bg-neutral-900 px-1.5 py-0.5 rounded text-xs">rewind_agent.thread("id")</code> to create them.
          </p>
        </div>
      </div>
    )
  }

  return (
    <div className="flex flex-col h-full">
      <div className="border-b border-neutral-800 px-4 py-3">
        <h2 className="text-base font-semibold text-neutral-200 flex items-center gap-2">
          <MessageSquare size={18} />
          Conversation Threads
        </h2>
      </div>

      <div className="flex-1 overflow-auto p-4 space-y-2">
        {threads.map((t) => (
          <ThreadCard key={t.thread_id} thread={t} onClick={() => selectThread(t.thread_id)} />
        ))}
      </div>
    </div>
  )
}

function ThreadCard({ thread, onClick }: { thread: ThreadSummary; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="w-full text-left p-4 rounded-lg border border-neutral-800 bg-neutral-900/50 hover:bg-neutral-800/50 transition-colors group"
    >
      <div className="flex items-center gap-2 mb-2">
        <MessageSquare size={14} className="text-cyan-400" />
        <span className="text-sm font-medium text-neutral-200 truncate">{thread.thread_id}</span>
      </div>
      <div className="flex items-center gap-4 text-xs text-neutral-500">
        <span>{thread.session_count} session{thread.session_count !== 1 ? 's' : ''}</span>
        <span>{thread.total_steps} steps</span>
        <span>{formatTokens(thread.total_tokens)} tokens</span>
      </div>
    </button>
  )
}
