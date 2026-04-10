import { useStore } from '@/hooks/use-store'
import { cn } from '@/lib/utils'
import { Database, FlaskConical, GitCompareArrows } from 'lucide-react'
import { DatasetBrowser } from '@/components/DatasetBrowser'
import { ExperimentList } from '@/components/ExperimentList'
import { ExperimentComparison } from '@/components/ExperimentComparison'

const tabs = [
  { key: 'datasets' as const, label: 'Datasets', icon: Database },
  { key: 'experiments' as const, label: 'Experiments', icon: FlaskConical },
  { key: 'compare' as const, label: 'Compare', icon: GitCompareArrows },
]

export function EvalDashboard() {
  const { evalTab, setEvalTab } = useStore()

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center gap-1 px-4 py-2 border-b border-neutral-800 bg-neutral-950">
        {tabs.map((tab) => (
          <button
            key={tab.key}
            onClick={() => setEvalTab(tab.key)}
            className={cn(
              'flex items-center gap-1.5 px-3 py-1.5 rounded-md text-xs font-medium transition-colors',
              evalTab === tab.key
                ? 'bg-neutral-800 text-neutral-100'
                : 'text-neutral-500 hover:text-neutral-300 hover:bg-neutral-900'
            )}
          >
            <tab.icon size={14} />
            {tab.label}
          </button>
        ))}
      </div>

      <div className="flex-1 overflow-hidden">
        {evalTab === 'datasets' && <DatasetBrowser />}
        {evalTab === 'experiments' && <ExperimentList />}
        {evalTab === 'compare' && <ExperimentComparison />}
      </div>
    </div>
  )
}
