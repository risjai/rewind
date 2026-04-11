import { SpanNode } from './SpanNode'
import type { SpanResponse } from '@/types/api'

interface SpanTreeProps {
  spans: SpanResponse[]
  selectedStepId: string | null
  onSelectStep: (id: string | null) => void
}

export function SpanTree({ spans, selectedStepId, onSelectStep }: SpanTreeProps) {
  if (spans.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm">
        No span tree available
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto scrollbar-thin p-2">
      {spans.map((span) => (
        <SpanNode
          key={span.id}
          span={span}
          depth={0}
          selectedStepId={selectedStepId}
          onSelectStep={onSelectStep}
        />
      ))}
    </div>
  )
}
