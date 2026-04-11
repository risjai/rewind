import { useState } from 'react'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import {
  ChevronDown, ChevronRight, CheckCircle2, XCircle, Loader2,
  Brain, Wrench, ClipboardList, Bot, ArrowRightLeft, Package
} from 'lucide-react'
import type { SpanResponse, StepResponse } from '@/types/api'

interface SpanNodeProps {
  span: SpanResponse
  depth: number
  selectedStepId: string | null
  onSelectStep: (id: string | null) => void
}

function SpanTypeIcon({ type }: { type: string }) {
  switch (type) {
    case 'agent': return <Bot size={14} className="text-cyan-400" />
    case 'tool': return <Wrench size={14} className="text-amber-400" />
    case 'handoff': return <ArrowRightLeft size={14} className="text-purple-400" />
    case 'custom': return <Package size={14} className="text-blue-400" />
    default: return <Package size={14} className="text-neutral-500" />
  }
}

function SpanStatusIcon({ status }: { status: string }) {
  switch (status) {
    case 'completed': return <CheckCircle2 size={11} className="text-green-500" />
    case 'error': return <XCircle size={11} className="text-red-500" />
    default: return <Loader2 size={11} className="text-amber-400 animate-spin" />
  }
}

function StepTypeIcon({ type }: { type: string }) {
  switch (type) {
    case 'llm_call': return <Brain size={13} className="text-purple-400" />
    case 'tool_call': return <Wrench size={13} className="text-amber-400" />
    case 'tool_result': return <ClipboardList size={13} className="text-blue-400" />
    default: return <Brain size={13} className="text-neutral-500" />
  }
}

export function SpanNode({ span, depth, selectedStepId, onSelectStep }: SpanNodeProps) {
  const [expanded, setExpanded] = useState(depth < 2)
  const hasChildren = span.child_spans.length > 0 || span.steps.length > 0
  const totalTokens = span.steps.reduce((sum, s) => sum + s.tokens_in + s.tokens_out, 0)

  return (
    <div className="select-none" style={{ paddingLeft: depth > 0 ? 16 : 0 }}>
      <button
        onClick={() => hasChildren && setExpanded(!expanded)}
        className={cn(
          'w-full text-left flex items-center gap-1.5 px-2 py-1.5 rounded-md transition-colors group',
          'hover:bg-neutral-800/50',
          hasChildren ? 'cursor-pointer' : 'cursor-default'
        )}
      >
        {hasChildren ? (
          expanded
            ? <ChevronDown size={12} className="text-neutral-500 shrink-0" />
            : <ChevronRight size={12} className="text-neutral-500 shrink-0" />
        ) : (
          <span className="w-3 shrink-0" />
        )}

        <SpanStatusIcon status={span.status} />
        <SpanTypeIcon type={span.span_type} />

        <span className="text-xs font-medium text-neutral-200 truncate">
          {span.name}
        </span>

        <span className="text-[10px] text-neutral-600 ml-1">
          {span.span_type}
        </span>

        <span className="ml-auto flex items-center gap-2 text-[10px] text-neutral-500 shrink-0">
          {span.duration_ms > 0 && (
            <span className="text-amber-500/70">{formatDuration(span.duration_ms)}</span>
          )}
          {totalTokens > 0 && (
            <span className="text-blue-500/70">{formatTokens(totalTokens)} tok</span>
          )}
        </span>
      </button>

      {span.error && (
        <div className="ml-8 px-2 py-1 text-[10px] text-red-400 truncate">
          ERROR: {span.error}
        </div>
      )}

      {expanded && hasChildren && (
        <div className="border-l border-neutral-800/50 ml-[17px]">
          {span.steps.map((step) => (
            <StepRow
              key={step.id}
              step={step}
              selected={step.id === selectedStepId}
              onSelect={() => onSelectStep(step.id === selectedStepId ? null : step.id)}
            />
          ))}
          {span.child_spans.map((child) => (
            <SpanNode
              key={child.id}
              span={child}
              depth={depth + 1}
              selectedStepId={selectedStepId}
              onSelectStep={onSelectStep}
            />
          ))}
        </div>
      )}
    </div>
  )
}

function StepRow({ step, selected, onSelect }: { step: StepResponse; selected: boolean; onSelect: () => void }) {
  return (
    <button
      onClick={onSelect}
      className={cn(
        'w-full text-left flex items-center gap-1.5 px-2 py-1.5 rounded-md transition-colors',
        selected ? 'bg-neutral-800' : 'hover:bg-neutral-900/60'
      )}
    >
      <span className="w-3 shrink-0" />
      <StepTypeIcon type={step.step_type} />

      <span className="text-[11px] font-mono text-neutral-500">#{step.step_number}</span>

      <span className="text-xs text-neutral-300 truncate">{step.model}</span>

      <span className="ml-auto flex items-center gap-2 text-[10px] text-neutral-500 shrink-0">
        {step.duration_ms > 0 && (
          <span className="text-amber-500/70">{formatDuration(step.duration_ms)}</span>
        )}
        {(step.tokens_in > 0 || step.tokens_out > 0) && (
          <span className="text-blue-500/70">{formatTokens(step.tokens_in)}↓ {formatTokens(step.tokens_out)}↑</span>
        )}
        {step.error && <span className="text-red-400">err</span>}
      </span>
    </button>
  )
}
