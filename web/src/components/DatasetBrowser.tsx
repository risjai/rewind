import { useQuery } from '@tanstack/react-query'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import { cn, timeAgo } from '@/lib/utils'
import { Database, ChevronRight } from 'lucide-react'
import type { EvalDataset } from '@/types/api'

export function DatasetBrowser() {
  const { selectedDatasetName, selectDataset } = useStore()

  const { data: datasets = [], isLoading } = useQuery({
    queryKey: ['eval-datasets'],
    queryFn: api.evalDatasets,
  })

  const { data: detail } = useQuery({
    queryKey: ['eval-dataset', selectedDatasetName],
    queryFn: () => api.evalDataset(selectedDatasetName!),
    enabled: !!selectedDatasetName,
  })

  return (
    <div className="flex h-full">
      <div className="w-80 border-r border-neutral-800 flex flex-col">
        <div className="px-4 py-3 border-b border-neutral-800 flex items-center gap-2">
          <Database size={16} className="text-neutral-400" />
          <h2 className="text-sm font-semibold text-neutral-200">Datasets</h2>
        </div>
        <div className="flex-1 overflow-auto scrollbar-thin">
          {isLoading ? (
            <div className="text-center text-neutral-500 text-sm py-8">Loading...</div>
          ) : datasets.length === 0 ? (
            <div className="text-center text-neutral-500 text-sm py-8">
              <p>No datasets yet</p>
              <p className="text-xs mt-1">Create a dataset with the eval SDK</p>
            </div>
          ) : (
            datasets.map((d) => (
              <DatasetListItem
                key={d.id}
                dataset={d}
                selected={selectedDatasetName === d.name}
                onClick={() => selectDataset(d.name)}
              />
            ))
          )}
        </div>
      </div>

      <div className="flex-1">
        {detail ? (
          <DatasetDetailView dataset={detail.dataset} examples={detail.examples} />
        ) : (
          <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
            Select a dataset to view
          </div>
        )}
      </div>
    </div>
  )
}

function DatasetListItem({ dataset, selected, onClick }: { dataset: EvalDataset; selected: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={cn(
        'w-full text-left px-4 py-3 border-b border-neutral-800/50 transition-colors',
        selected ? 'bg-neutral-800/80' : 'hover:bg-neutral-900/60'
      )}
    >
      <div className="flex items-center justify-between">
        <span className="text-sm font-medium text-neutral-200">{dataset.name}</span>
        <ChevronRight size={14} className="text-neutral-600" />
      </div>
      <div className="flex items-center gap-3 text-xs text-neutral-500 mt-1">
        <span>v{dataset.version}</span>
        <span>{dataset.example_count} examples</span>
        <span>{timeAgo(dataset.updated_at)}</span>
      </div>
      {dataset.description && <p className="text-xs text-neutral-600 mt-1 truncate">{dataset.description}</p>}
    </button>
  )
}

function DatasetDetailView({ dataset, examples }: { dataset: EvalDataset; examples: { id: string; ordinal: number; input: unknown; expected: unknown; metadata: Record<string, unknown> }[] }) {
  return (
    <div className="flex flex-col h-full">
      <div className="px-4 py-3 border-b border-neutral-800">
        <h3 className="text-sm font-semibold text-neutral-200">{dataset.name}</h3>
        <div className="flex items-center gap-4 text-xs text-neutral-500 mt-1">
          <span>Version {dataset.version}</span>
          <span>{dataset.example_count} examples</span>
          <span>{timeAgo(dataset.updated_at)}</span>
        </div>
        {dataset.description && <p className="text-xs text-neutral-400 mt-1">{dataset.description}</p>}
      </div>

      <div className="flex-1 overflow-auto scrollbar-thin">
        <table className="w-full text-xs">
          <thead className="sticky top-0 bg-neutral-950">
            <tr className="text-neutral-500 border-b border-neutral-800">
              <th className="text-left px-4 py-2 font-medium w-12">#</th>
              <th className="text-left px-4 py-2 font-medium">Input</th>
              <th className="text-left px-4 py-2 font-medium">Expected</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-neutral-800/50">
            {examples.map((ex) => (
              <tr key={ex.id} className="text-neutral-300 hover:bg-neutral-900/60">
                <td className="px-4 py-2 font-mono text-neutral-500">{ex.ordinal}</td>
                <td className="px-4 py-2 max-w-xs">
                  <span className="block truncate font-mono text-neutral-300">{truncateJson(ex.input)}</span>
                </td>
                <td className="px-4 py-2 max-w-xs">
                  <span className="block truncate font-mono text-neutral-400">{truncateJson(ex.expected)}</span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function truncateJson(value: unknown): string {
  if (value === null || value === undefined) return '--'
  const s = typeof value === 'string' ? value : JSON.stringify(value)
  return s.length > 120 ? s.slice(0, 120) + '...' : s
}
