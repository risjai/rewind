import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn, timeAgo, formatTokens } from '@/lib/utils'
import {
  PanelLeftClose, PanelLeftOpen, Activity,
  Shield, ChevronRight, Sun, Moon, FlaskConical, MessageSquare, Plug,
} from 'lucide-react'
import { useTheme } from '@/hooks/use-theme'
import type { Session } from '@/types/api'

export function Sidebar() {
  const { sidebarCollapsed, toggleSidebar, selectedSessionId, selectSession, setView, view } = useStore()

  const { data: sessions = [], isLoading } = useQuery({
    queryKey: ['sessions'],
    queryFn: api.sessions,
    refetchInterval: 5000,
  })

  if (sidebarCollapsed) {
    return (
      <aside className="fixed left-0 top-0 h-full w-12 bg-neutral-950 border-r border-neutral-800 flex flex-col items-center py-3 z-30">
        <button onClick={toggleSidebar} className="p-1.5 rounded hover:bg-neutral-800 text-neutral-400 hover:text-neutral-200">
          <PanelLeftOpen size={18} />
        </button>
        <div className="mt-4 space-y-2">
          <NavIconButton icon={Activity} active={view === 'sessions'} onClick={() => setView('sessions')} title="Sessions" />
          <NavIconButton icon={Shield} active={view === 'baselines'} onClick={() => setView('baselines')} title="Baselines" />
          <NavIconButton icon={FlaskConical} active={view === 'evaluations'} onClick={() => setView('evaluations')} title="Evaluations" />
          <NavIconButton icon={MessageSquare} active={view === 'threads'} onClick={() => setView('threads')} title="Threads" />
        </div>
      </aside>
    )
  }

  return (
    <aside className="fixed left-0 top-0 h-full w-72 bg-neutral-950 border-r border-neutral-800 flex flex-col z-30">
      <div className="flex items-center justify-between px-4 py-3 border-b border-neutral-800">
        <div className="flex items-center gap-2">
          <span className="text-lg">⏪</span>
          <span className="font-semibold text-sm text-neutral-200">Rewind</span>
        </div>
        <button onClick={toggleSidebar} className="p-1.5 rounded hover:bg-neutral-800 text-neutral-400 hover:text-neutral-200">
          <PanelLeftClose size={18} />
        </button>
      </div>

      <nav className="flex gap-1 px-3 pt-3">
        <NavButton icon={Activity} label="Sessions" active={view === 'sessions'} onClick={() => setView('sessions')} />
        <NavButton icon={Shield} label="Baselines" active={view === 'baselines'} onClick={() => setView('baselines')} />
        <NavButton icon={FlaskConical} label="Evals" active={view === 'evaluations'} onClick={() => setView('evaluations')} />
        <NavButton icon={MessageSquare} label="Threads" active={view === 'threads'} onClick={() => setView('threads')} />
      </nav>

      <div className="flex-1 overflow-y-auto scrollbar-thin px-2 py-2 space-y-0.5">
        {isLoading ? (
          <div className="text-center text-neutral-500 text-xs py-8">Loading...</div>
        ) : sessions.length === 0 ? (
          <div className="text-center text-neutral-500 text-xs py-8">
            No sessions yet
          </div>
        ) : (
          sessions.map((s) => (
            <SessionItem
              key={s.id}
              session={s}
              selected={selectedSessionId === s.id}
              onClick={() => selectSession(s.id)}
            />
          ))
        )}
      </div>

      <div className="px-4 py-2 border-t border-neutral-800 flex items-center justify-between">
        <span className="text-xs text-neutral-600">{sessions.length} session{sessions.length !== 1 ? 's' : ''}</span>
        <ThemeToggle />
      </div>
    </aside>
  )
}

function SessionItem({ session, selected, onClick }: { session: Session; selected: boolean; onClick: () => void }) {
  const isLive = session.status === 'Recording'
  const isHook = session.source === 'hooks'

  return (
    <button
      onClick={onClick}
      className={cn(
        'w-full text-left px-3 py-2 rounded-lg flex items-center gap-2 group transition-colors',
        selected ? 'bg-neutral-800 text-neutral-100' : 'text-neutral-400 hover:bg-neutral-900 hover:text-neutral-200'
      )}
    >
      <StatusDot status={session.status} />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5">
          <span className="text-sm font-medium truncate">{session.name}</span>
          {isLive && <span className="text-[10px] font-semibold text-cyan-400 uppercase tracking-wide">live</span>}
          {isHook && (
            <span className="flex items-center gap-0.5 text-[10px] font-semibold text-violet-400 uppercase tracking-wide">
              <Plug size={10} />
              hooks
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 text-xs text-neutral-500 mt-0.5">
          <span>{session.total_steps} steps</span>
          {session.total_tokens > 0 && <span>{formatTokens(session.total_tokens)} tokens</span>}
          {(session.metadata?.cache_tokens as number) > 0 && (
            <span className="text-neutral-600">{formatTokens(session.metadata.cache_tokens as number)} cached</span>
          )}
          <span>{timeAgo(session.created_at)}</span>
        </div>
      </div>
      <ChevronRight size={14} className="text-neutral-600 opacity-0 group-hover:opacity-100 transition-opacity" />
    </button>
  )
}

function StatusDot({ status }: { status: Session['status'] }) {
  const colors: Record<string, string> = {
    Recording: 'bg-cyan-400 animate-pulse-dot',
    Completed: 'bg-green-500',
    Failed: 'bg-red-500',
    Forked: 'bg-amber-500',
  }
  return <span className={cn('w-2 h-2 rounded-full shrink-0', colors[status] || 'bg-neutral-600')} />
}

function NavButton({ icon: Icon, label, active, onClick }: { icon: React.ComponentType<{ size?: number }>; label: string; active: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        'flex items-center gap-1.5 px-2.5 py-1.5 rounded-md text-xs font-medium transition-colors',
        active ? 'bg-neutral-800 text-neutral-100' : 'text-neutral-500 hover:text-neutral-300 hover:bg-neutral-900'
      )}
    >
      <Icon size={14} />
      {label}
    </button>
  )
}

function ThemeToggle() {
  const { theme, toggle } = useTheme()
  return (
    <button onClick={toggle} className="p-1 rounded hover:bg-neutral-800 text-neutral-500 hover:text-neutral-300 transition-colors" title={`Switch to ${theme === 'dark' ? 'light' : 'dark'} mode`}>
      {theme === 'dark' ? <Sun size={14} /> : <Moon size={14} />}
    </button>
  )
}

function NavIconButton({ icon: Icon, active, onClick, title }: { icon: React.ComponentType<{ size?: number }>; active: boolean; onClick: () => void; title: string }) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={cn(
        'p-1.5 rounded transition-colors',
        active ? 'bg-neutral-800 text-neutral-100' : 'text-neutral-500 hover:text-neutral-300 hover:bg-neutral-900'
      )}
    >
      <Icon size={18} />
    </button>
  )
}
