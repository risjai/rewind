import { useRef, useEffect } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import {
  CheckCircle2, XCircle, Loader2, Brain, Wrench, ClipboardList,
  Eye, Pencil, FileText, Terminal, Search, Bot, Globe, ListTodo, Plug, MessageSquare, Zap,
} from 'lucide-react'
import type { StepResponse } from '@/types/api'

interface StepTimelineProps {
  steps: StepResponse[]
  selectedStepId: string | null
  onSelectStep: (id: string | null) => void
  autoFollow?: boolean
}

export function StepTimeline({ steps, selectedStepId, onSelectStep, autoFollow }: StepTimelineProps) {
  const parentRef = useRef<HTMLDivElement>(null)

  const virtualizer = useVirtualizer({
    count: steps.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 64,
    overscan: 10,
  })

  useEffect(() => {
    if (autoFollow && steps.length > 0) {
      virtualizer.scrollToIndex(steps.length - 1, { align: 'end' })
    }
  }, [steps.length, autoFollow, virtualizer])

  return (
    <div ref={parentRef} className="flex-1 overflow-auto scrollbar-thin">
      <div style={{ height: virtualizer.getTotalSize(), width: '100%', position: 'relative' }}>
        {virtualizer.getVirtualItems().map((virtualItem) => {
          const step = steps[virtualItem.index]
          const selected = step.id === selectedStepId

          return (
            <div
              key={virtualItem.key}
              data-index={virtualItem.index}
              ref={virtualizer.measureElement}
              style={{
                position: 'absolute',
                top: 0,
                left: 0,
                width: '100%',
                transform: `translateY(${virtualItem.start}px)`,
              }}
            >
              <button
                onClick={() => onSelectStep(selected ? null : step.id)}
                className={cn(
                  'w-full text-left px-3 py-2.5 flex items-start gap-2.5 border-b border-neutral-800/50 transition-colors',
                  selected ? 'bg-neutral-800/80' : 'hover:bg-neutral-900/60'
                )}
              >
                <div className="flex flex-col items-center gap-1 pt-0.5">
                  {step.tool_name ? <ToolNameIcon toolName={step.tool_name} /> : <StepTypeIcon type={step.step_type} />}
                  <StatusIcon status={step.status} />
                </div>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-[11px] font-mono text-neutral-500">#{step.step_number}</span>
                    <span className="text-xs font-medium text-neutral-200">{toolNameLabel(step)}</span>
                    {step.model && (
                      <span className="text-[10px] bg-neutral-800 text-neutral-400 px-1.5 py-0.5 rounded font-mono">{step.model}</span>
                    )}
                  </div>
                  <div className="flex items-center gap-3 mt-1 text-[11px] text-neutral-500">
                    {step.duration_ms > 0 && <span>{formatDuration(step.duration_ms)}</span>}
                    {(step.tokens_in > 0 || step.tokens_out > 0) && (
                      <span>{formatTokens(step.tokens_in)} in / {formatTokens(step.tokens_out)} out</span>
                    )}
                    {step.error && <span className="text-red-400 truncate">{step.error}</span>}
                  </div>
                  {step.response_preview && (
                    <p className="text-xs text-neutral-500 mt-1 truncate leading-relaxed">{step.response_preview}</p>
                  )}
                </div>
              </button>
            </div>
          )
        })}
      </div>
    </div>
  )
}

function StepTypeIcon({ type }: { type: string }) {
  switch (type) {
    case 'llm_call': return <Brain size={14} className="text-purple-400" />
    case 'tool_call': return <Wrench size={14} className="text-amber-400" />
    case 'tool_result': return <ClipboardList size={14} className="text-blue-400" />
    default: return <Brain size={14} className="text-neutral-500" />
  }
}

function StatusIcon({ status }: { status: string }) {
  switch (status) {
    case 'success': return <CheckCircle2 size={12} className="text-green-500" />
    case 'error': return <XCircle size={12} className="text-red-500" />
    default: return <Loader2 size={12} className="text-amber-400 animate-spin" />
  }
}

function ToolNameIcon({ toolName }: { toolName: string }) {
  if (toolName.startsWith('mcp__')) return <Plug size={14} className="text-violet-400" />
  switch (toolName) {
    case 'Read': return <Eye size={14} className="text-blue-400" />
    case 'Edit': return <Pencil size={14} className="text-amber-400" />
    case 'Write': return <FileText size={14} className="text-green-400" />
    case 'Bash': return <Terminal size={14} className="text-emerald-400" />
    case 'Grep':
    case 'Glob': return <Search size={14} className="text-cyan-400" />
    case 'Agent': return <Bot size={14} className="text-cyan-400" />
    case 'WebFetch': return <Globe size={14} className="text-blue-400" />
    case 'TodoWrite': return <ListTodo size={14} className="text-orange-400" />
    case 'user_prompt': return <MessageSquare size={14} className="text-cyan-400" />
    case 'hook_event': return <Zap size={14} className="text-yellow-400" />
    default: return <Wrench size={14} className="text-amber-400" />
  }
}

function toolNameLabel(step: StepResponse): string {
  if (step.tool_name) {
    if (step.tool_name === 'user_prompt') return 'User Prompt'
    if (step.tool_name === 'hook_event') return 'Hook Event'
    if (step.tool_name.startsWith('mcp__')) return step.tool_name
    return step.tool_name
  }
  return step.step_type_label
}
