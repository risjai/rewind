import { Sidebar } from '@/components/Sidebar'
import { SessionView } from '@/components/SessionView'
import { DiffView } from '@/components/DiffView'
import { BaselinesView } from '@/components/BaselinesView'
import { EvalDashboard } from '@/components/EvalDashboard'
import { ErrorBoundary } from '@/components/ErrorBoundary'
import { useStore } from '@/hooks/use-store'

export function App() {
  const { selectedSessionId, sidebarCollapsed, view } = useStore()

  return (
    <ErrorBoundary>
    <div className="flex h-screen overflow-hidden">
      <Sidebar />
      <main className={`flex-1 overflow-hidden transition-all ${sidebarCollapsed ? 'ml-12' : 'ml-72'}`}>
        {view === 'sessions' && selectedSessionId ? (
          <SessionView sessionId={selectedSessionId} />
        ) : view === 'diff' && selectedSessionId ? (
          <DiffView sessionId={selectedSessionId} />
        ) : view === 'baselines' ? (
          <BaselinesView />
        ) : view === 'evaluations' ? (
          <EvalDashboard />
        ) : (
          <EmptyState />
        )}
      </main>
    </div>
    </ErrorBoundary>
  )
}

function EmptyState() {
  return (
    <div className="flex items-center justify-center h-full text-neutral-500">
      <div className="text-center space-y-3">
        <div className="text-5xl">⏪</div>
        <h2 className="text-xl font-semibold text-neutral-300">Rewind</h2>
        <p className="text-sm">Select a session to inspect, or run <code className="text-cyan-400 bg-neutral-900 px-1.5 py-0.5 rounded text-xs">rewind demo</code> to create sample data.</p>
      </div>
    </div>
  )
}
