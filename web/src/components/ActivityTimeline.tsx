import { useMemo, useRef, useCallback, useReducer, useEffect, useState } from 'react'
import { cn, formatDuration, formatTokens } from '@/lib/utils'
import { Brain, Wrench, ClipboardList, MessageSquare, Radio, GitBranch, Play } from 'lucide-react'
import type { SpanResponse, StepResponse, Session } from '@/types/api'

// --- Lane building logic (exported for testing) ---

export interface Lane {
  id: string
  label: string
  isSubLane: boolean
  steps: StepResponse[]
  color: string
  positionMode: 'created_at' | 'step_number'
}

const LANE_COLORS = [
  'bg-cyan-500', 'bg-amber-500', 'bg-purple-500', 'bg-emerald-500',
  'bg-rose-500', 'bg-blue-500', 'bg-orange-500', 'bg-teal-500',
]

const TOOL_COLORS: Record<string, string> = {
  Read: 'bg-emerald-500',
  Write: 'bg-blue-500',
  Edit: 'bg-blue-400',
  Shell: 'bg-amber-500',
  Bash: 'bg-amber-500',
  Grep: 'bg-teal-500',
  Glob: 'bg-teal-400',
  Search: 'bg-violet-500',
  WebSearch: 'bg-violet-500',
  Agent: 'bg-cyan-500',
}

function getToolColor(toolName: string): string {
  return TOOL_COLORS[toolName] || LANE_COLORS[hashString(toolName) % LANE_COLORS.length]
}

function hashString(s: string): number {
  let hash = 0
  for (let i = 0; i < s.length; i++) {
    hash = ((hash << 5) - hash + s.charCodeAt(i)) | 0
  }
  return Math.abs(hash)
}

function collectSpanSteps(span: SpanResponse): StepResponse[] {
  const steps = [...span.steps]
  for (const child of span.child_spans) {
    if (child.span_type !== 'agent') {
      steps.push(...collectSpanSteps(child))
    }
  }
  return steps
}

function flattenAgentSpans(
  spans: SpanResponse[],
  parentIsAgent: boolean,
  depth: number = 0,
): { span: SpanResponse; isSubLane: boolean }[] {
  if (depth > 20) return []
  const result: { span: SpanResponse; isSubLane: boolean }[] = []
  for (const span of spans) {
    if (span.span_type === 'agent' || (!parentIsAgent && span.parent_span_id === null)) {
      result.push({ span, isSubLane: parentIsAgent })
      result.push(...flattenAgentSpans(span.child_spans, true, depth + 1))
    } else if (span.parent_span_id === null) {
      result.push({ span, isSubLane: false })
      result.push(...flattenAgentSpans(span.child_spans, true, depth + 1))
    }
  }
  return result
}

export function buildLanes(
  spans: SpanResponse[],
  steps: StepResponse[],
  session: Session,
): Lane[] {
  if (spans.length > 0) {
    const agentEntries = flattenAgentSpans(spans, false)
    if (agentEntries.length === 0 && steps.length === 0) return []

    const lanes: Lane[] = agentEntries.map(({ span, isSubLane }, i) => ({
      id: span.id,
      label: span.name,
      isSubLane,
      steps: collectSpanSteps(span).sort((a, b) => a.step_number - b.step_number),
      color: LANE_COLORS[i % LANE_COLORS.length],
      positionMode: 'created_at' as const,
    }))

    const assignedStepIds = new Set(lanes.flatMap(l => l.steps.map(s => s.id)))
    const unassigned = steps.filter(s => !assignedStepIds.has(s.id))
    if (unassigned.length > 0) {
      if (lanes.length > 0) {
        lanes[0].steps.push(...unassigned)
        lanes[0].steps.sort((a, b) => a.step_number - b.step_number)
      } else {
        lanes.push({
          id: 'unassigned',
          label: 'Main',
          isSubLane: false,
          steps: unassigned.sort((a, b) => a.step_number - b.step_number),
          color: LANE_COLORS[0],
          positionMode: 'created_at',
        })
      }
    }

    return lanes
  }

  if (steps.length === 0) return []

  // Group steps by type: LLM calls, prompts, and each tool in its own lane.
  // This applies to all spanless sessions (hooks, api, direct, otel_import).
  const groups: Record<string, StepResponse[]> = {}
  const order: string[] = []

  for (const step of steps) {
    let key: string
    if (step.step_type === 'llm_call') key = 'LLM Calls'
    else if (step.step_type === 'user_prompt') key = 'Prompts'
    else if (step.tool_name) key = step.tool_name
    else key = step.step_type_label || 'Other'

    if (!groups[key]) {
      groups[key] = []
      order.push(key)
    }
    groups[key].push(step)
  }

  const priorityOrder = ['LLM Calls', 'Prompts']
  const sorted = [
    ...priorityOrder.filter(k => groups[k]),
    ...order.filter(k => !priorityOrder.includes(k)),
  ]

  return sorted.map((key, i) => ({
    id: `lane-${key}`,
    label: key,
    isSubLane: !['LLM Calls', 'Prompts'].includes(key),
    steps: groups[key],
    color: key === 'LLM Calls' ? 'bg-purple-500'
      : key === 'Prompts' ? 'bg-cyan-500'
      : getToolColor(key),
    positionMode: 'step_number' as const,
  }))
}

// --- Viewport state (zoom/pan) ---

export interface ViewportState {
  zoom: number
  offset: number
  focusedLaneIndex: number | null
}

type ViewportAction =
  | { type: 'zoom_in' }
  | { type: 'zoom_out' }
  | { type: 'reset' }
  | { type: 'pan'; delta: number }
  | { type: 'set_offset'; offset: number }
  | { type: 'focus_lane'; index: number | null }
  | { type: 'wheel_zoom'; deltaY: number; cursorFraction: number; totalRange: number }

const ZOOM_FACTOR = 1.25
const MIN_ZOOM = 1
const MAX_ZOOM = 50

export function viewportReducer(state: ViewportState, action: ViewportAction): ViewportState {
  switch (action.type) {
    case 'zoom_in': {
      const zoom = Math.min(MAX_ZOOM, state.zoom * ZOOM_FACTOR)
      return { ...state, zoom }
    }
    case 'zoom_out': {
      const zoom = Math.max(MIN_ZOOM, state.zoom / ZOOM_FACTOR)
      return { ...state, zoom }
    }
    case 'reset':
      return { ...state, zoom: 1, offset: 0 }
    case 'pan':
      return { ...state, offset: Math.max(0, state.offset + action.delta) }
    case 'set_offset':
      return { ...state, offset: action.offset }
    case 'focus_lane':
      return { ...state, focusedLaneIndex: action.index }
    case 'wheel_zoom': {
      const direction = action.deltaY < 0 ? 1 : -1
      const newZoom = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, state.zoom * (direction > 0 ? ZOOM_FACTOR : 1 / ZOOM_FACTOR)))
      if (newZoom === state.zoom) return state
      const visibleRange = action.totalRange / state.zoom
      const cursorPos = state.offset + action.cursorFraction * visibleRange
      const newVisibleRange = action.totalRange / newZoom
      const newOffset = cursorPos - action.cursorFraction * newVisibleRange
      return { ...state, zoom: newZoom, offset: Math.max(0, newOffset) }
    }
    default:
      return state
  }
}

const INITIAL_VIEWPORT: ViewportState = { zoom: 1, offset: 0, focusedLaneIndex: null }

// --- Bar positioning ---

interface BarLayout {
  step: StepResponse
  leftPct: number
  widthPct: number
}

export function computeBarLayouts(
  steps: StepResponse[],
  positionMode: 'created_at' | 'step_number',
  sessionBounds: { startMs: number; endMs: number; maxStep: number },
  axisMode: AxisMode = 'duration',
): BarLayout[] {
  if (steps.length === 0) return []

  if (positionMode === 'step_number') {
    const { maxStep } = sessionBounds
    if (maxStep <= 0) return []
    return steps.map(step => {
      // Cost is a rough visual estimate for relative sizing; production pricing comes from backend pricing.rs
      const metric = axisMode === 'tokens' ? step.tokens_in + step.tokens_out
        : axisMode === 'cost' ? (step.tokens_in + step.tokens_out * 3)
        : step.duration_ms
      return {
        step,
        leftPct: ((step.step_number - 1) / maxStep) * 100,
        widthPct: Math.max(0.4, (1 / maxStep) * 100 * Math.min(1, (metric > 0 ? 0.3 + metric / (metric + 100) : 0.3))),
      }
    })
  }

  const { startMs, endMs } = sessionBounds
  const totalMs = endMs - startMs
  if (totalMs <= 0) return steps.map((step, i) => ({
    step,
    leftPct: steps.length > 1 ? (i / (steps.length - 1)) * 90 : 0,
    widthPct: Math.max(0.5, 90 / Math.max(1, steps.length)),
  }))

  return steps.map(step => {
    const stepStart = new Date(step.created_at).getTime()
    const leftPct = ((stepStart - startMs) / totalMs) * 100
    const metric = axisMode === 'tokens' ? step.tokens_in + step.tokens_out
      : axisMode === 'cost' ? (step.tokens_in + step.tokens_out * 3)
      : step.duration_ms
    const widthPct = axisMode === 'duration'
      ? Math.max(0.3, (step.duration_ms / totalMs) * 100)
      : Math.max(0.3, (metric / Math.max(1, totalMs)) * 100 * 50)
    return { step, leftPct, widthPct: Math.min(widthPct, 30) }
  })
}

// --- Step type icon ---

function StepTypeIcon({ type }: { type: string }) {
  switch (type) {
    case 'llm_call': return <Brain size={10} className="text-purple-300" />
    case 'tool_call': return <Wrench size={10} className="text-amber-300" />
    case 'tool_result': return <ClipboardList size={10} className="text-blue-300" />
    case 'user_prompt': return <MessageSquare size={10} className="text-cyan-300" />
    default: return <Radio size={10} className="text-neutral-400" />
  }
}

// --- Component ---

export type AxisMode = 'duration' | 'tokens' | 'cost'

interface ActivityTimelineProps {
  spans: SpanResponse[]
  steps: StepResponse[]
  session: Session
  selectedStepId: string | null
  onSelectStep: (id: string | null) => void
  isLive?: boolean
  isCursor?: boolean
  onFork?: (step: StepResponse) => void
  onReplay?: (step: StepResponse) => void
}

const LANE_HEIGHT = 36
const LABEL_WIDTH = 160

export function ActivityTimeline({
  spans,
  steps,
  session,
  selectedStepId,
  onSelectStep,
  isLive,
  isCursor,
  onFork,
  onReplay,
}: ActivityTimelineProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const laneAreaRef = useRef<HTMLDivElement>(null)
  const isDragging = useRef(false)
  const dragStartX = useRef(0)
  const dragStartOffset = useRef(0)
  const prevStepCount = useRef(steps.length)

  const [viewport, dispatch] = useReducer(viewportReducer, INITIAL_VIEWPORT)
  const [autoFollow, setAutoFollow] = useState(true)
  const [axisMode, setAxisMode] = useState<AxisMode>('duration')
  const [analyticsLaneId, setAnalyticsLaneId] = useState<string | null>(null)

  const lanes = useMemo(() => buildLanes(spans, steps, session), [spans, steps, session])

  const sessionBounds = useMemo(() => {
    const allSteps = lanes.flatMap(l => l.steps)
    if (allSteps.length === 0) return { startMs: 0, endMs: 1, maxStep: 0 }

    return allSteps.reduce((acc, s) => {
      const t = new Date(s.created_at).getTime()
      return {
        startMs: Math.min(acc.startMs, t),
        endMs: Math.max(acc.endMs, t + s.duration_ms),
        maxStep: Math.max(acc.maxStep, s.step_number),
      }
    }, { startMs: Infinity, endMs: -Infinity, maxStep: 0 })
  }, [lanes])

  const totalRange = Math.max(1, lanes[0]?.positionMode === 'step_number'
    ? sessionBounds.maxStep
    : sessionBounds.endMs - sessionBounds.startMs)

  const handleBarClick = useCallback((stepId: string) => {
    onSelectStep(stepId === selectedStepId ? null : stepId)
    // Re-focus the container so keyboard navigation continues to work
    containerRef.current?.focus()
  }, [onSelectStep, selectedStepId])

  // Auto-follow: when new steps arrive during live recording, pan viewport to show them
  useEffect(() => {
    if (!isLive || !autoFollow) return
    if (steps.length > prevStepCount.current && totalRange > 0) {
      const visibleRange = totalRange / viewport.zoom
      const newOffset = Math.max(0, totalRange - visibleRange)
      dispatch({ type: 'set_offset', offset: newOffset })
    }
    prevStepCount.current = steps.length
  }, [steps.length, isLive, autoFollow, totalRange, viewport.zoom])

  // Wheel zoom handler — disables auto-follow
  const handleWheel = useCallback((e: React.WheelEvent) => {
    if (Math.abs(e.deltaY) < 4) return
    e.preventDefault()
    setAutoFollow(false)
    const rect = laneAreaRef.current?.getBoundingClientRect()
    if (!rect) return
    const cursorFraction = Math.max(0, Math.min(1, (e.clientX - rect.left - LABEL_WIDTH) / (rect.width - LABEL_WIDTH)))
    dispatch({ type: 'wheel_zoom', deltaY: e.deltaY, cursorFraction, totalRange })
  }, [totalRange])

  // Drag-to-pan handlers — disables auto-follow
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (e.button !== 0) return
    isDragging.current = true
    dragStartX.current = e.clientX
    dragStartOffset.current = viewport.offset
    if (laneAreaRef.current) laneAreaRef.current.style.cursor = 'grabbing'
    containerRef.current?.focus()
  }, [viewport.offset])

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!isDragging.current || !laneAreaRef.current) return
      setAutoFollow(false)
      const rect = laneAreaRef.current.getBoundingClientRect()
      const barAreaWidth = rect.width - LABEL_WIDTH
      if (barAreaWidth <= 0) return
      const pxDelta = dragStartX.current - e.clientX
      const visibleRange = totalRange / viewport.zoom
      const rangeDelta = (pxDelta / barAreaWidth) * visibleRange
      dispatch({ type: 'set_offset', offset: Math.max(0, dragStartOffset.current + rangeDelta) })
    }
    const onUp = () => {
      isDragging.current = false
      if (laneAreaRef.current) laneAreaRef.current.style.cursor = 'grab'
    }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
  }, [totalRange, viewport.zoom])

  // Keyboard navigation — arrows/vim keys match the visual layout:
  // ←/→ (h/l) = step navigation within focused lane (bars are horizontal)
  // ↑/↓ (k/j) = lane navigation (lanes stack vertically)
  // Shift+←/→ (H/L) = pan viewport
  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    const navSteps = viewport.focusedLaneIndex !== null && viewport.focusedLaneIndex < lanes.length
      ? lanes[viewport.focusedLaneIndex].steps
      : lanes.flatMap(l => l.steps)
    switch (e.key) {
      case '+': case '=': e.preventDefault(); dispatch({ type: 'zoom_in' }); break
      case '-': e.preventDefault(); dispatch({ type: 'zoom_out' }); break
      case '0': e.preventDefault(); dispatch({ type: 'reset' }); break
      case 'L': case 'H':
        e.preventDefault()
        dispatch({ type: 'pan', delta: (e.key === 'L' ? 1 : -1) * totalRange / viewport.zoom * 0.15 })
        break
      case 'l': case 'ArrowRight': {
        e.preventDefault()
        if (e.shiftKey) {
          dispatch({ type: 'pan', delta: totalRange / viewport.zoom * 0.15 })
        } else {
          const currentIdx = navSteps.findIndex(s => s.id === selectedStepId)
          if (currentIdx < navSteps.length - 1) onSelectStep(navSteps[currentIdx + 1].id)
          else if (currentIdx === -1 && navSteps.length > 0) onSelectStep(navSteps[0].id)
        }
        break
      }
      case 'h': case 'ArrowLeft': {
        e.preventDefault()
        if (e.shiftKey) {
          dispatch({ type: 'pan', delta: -totalRange / viewport.zoom * 0.15 })
        } else {
          const currentIdx = navSteps.findIndex(s => s.id === selectedStepId)
          if (currentIdx > 0) onSelectStep(navSteps[currentIdx - 1].id)
          else if (currentIdx === -1 && navSteps.length > 0) onSelectStep(navSteps[navSteps.length - 1].id)
        }
        break
      }
      case 'j': case 'ArrowDown': {
        e.preventDefault()
        const next = viewport.focusedLaneIndex !== null
          ? Math.min(lanes.length - 1, viewport.focusedLaneIndex + 1)
          : 0
        dispatch({ type: 'focus_lane', index: next })
        break
      }
      case 'k': case 'ArrowUp': {
        e.preventDefault()
        const prev = viewport.focusedLaneIndex !== null
          ? Math.max(0, viewport.focusedLaneIndex - 1)
          : 0
        dispatch({ type: 'focus_lane', index: prev })
        break
      }
      case 'Escape':
        e.preventDefault()
        onSelectStep(null)
        dispatch({ type: 'focus_lane', index: null })
        break
    }
  }, [lanes, selectedStepId, onSelectStep, viewport.zoom, viewport.focusedLaneIndex, totalRange])

  const minimapBars = useMemo(
    () => lanes.map(lane => ({ lane, bars: computeBarLayouts(lane.steps, lane.positionMode, sessionBounds) })),
    [lanes, sessionBounds],
  )

  if (lanes.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-neutral-500 text-sm py-8">
        No activity to display
      </div>
    )
  }

  const visibleRange = totalRange / viewport.zoom
  const viewStart = viewport.offset
  const viewEnd = viewStart + visibleRange

  function toViewPct(value: number): number {
    return ((value - viewStart) / visibleRange) * 100
  }

  return (
    <div
      className="flex flex-col overflow-hidden h-full focus:outline-none"
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
    >
      {/* Header */}
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-neutral-800 bg-neutral-900/50 shrink-0">
        <span className="text-[10px] uppercase tracking-wider font-semibold text-neutral-500">
          Activity Timeline
        </span>
        {isLive && (
          <span className="flex items-center gap-1 text-[10px] text-cyan-400">
            <span className="w-1.5 h-1.5 rounded-full bg-cyan-400 animate-pulse" />
            LIVE
          </span>
        )}
        {isLive && (
          <button
            onClick={() => setAutoFollow(!autoFollow)}
            className={cn(
              'text-[10px] px-1.5 py-0.5 rounded transition-colors',
              autoFollow
                ? 'bg-cyan-900/40 text-cyan-300 border border-cyan-700/50'
                : 'text-neutral-500 border border-neutral-700/50 hover:text-neutral-300'
            )}
          >
            {autoFollow ? 'Following' : 'Follow'}
          </button>
        )}
        {viewport.zoom > 1 && (
          <button
            onClick={() => dispatch({ type: 'reset' })}
            className="text-[10px] text-neutral-500 hover:text-neutral-300 transition-colors px-1.5 py-0.5 rounded border border-neutral-700/50 hover:border-neutral-600"
          >
            {viewport.zoom.toFixed(1)}x — Reset
          </button>
        )}

        {/* Axis mode selector */}
        <div className="flex items-center border border-neutral-700/50 rounded overflow-hidden ml-1">
          {(['duration', 'tokens', 'cost'] as const).map(mode => {
            const disabled = mode !== 'duration' && isCursor
            return (
              <button
                key={mode}
                onClick={() => !disabled && setAxisMode(mode)}
                disabled={disabled}
                title={disabled ? 'Token/cost data unavailable for Cursor sessions' : undefined}
                className={cn(
                  'text-[9px] px-1.5 py-0.5 transition-colors capitalize',
                  axisMode === mode
                    ? 'bg-neutral-700 text-neutral-100'
                    : disabled
                      ? 'text-neutral-700 cursor-not-allowed'
                      : 'text-neutral-500 hover:text-neutral-300'
                )}
              >
                {mode === 'cost' ? 'cost (est.)' : mode}
              </button>
            )
          })}
        </div>

        <span className="ml-auto text-[10px] text-neutral-600">
          {lanes.length} {lanes.length === 1 ? 'lane' : 'lanes'} · {steps.length} steps
          {(() => {
            const errorCount = steps.filter(s => s.status === 'error').length
            return errorCount > 0 ? <span className="text-red-400 ml-1">· {errorCount} errors</span> : null
          })()}
        </span>
      </div>

      {/* Minimap */}
      <Minimap
        lanes={lanes}
        minimapBars={minimapBars}
        totalRange={totalRange}
        viewport={viewport}
        onSetOffset={(offset) => dispatch({ type: 'set_offset', offset })}
      />

      {/* Lane Analytics Popover */}
      {analyticsLaneId && (() => {
        const lane = lanes.find(l => l.id === analyticsLaneId)
        return lane ? <LaneAnalytics lane={lane} onClose={() => setAnalyticsLaneId(null)} /> : null
      })()}

      {/* Swim lanes */}
      <div
        ref={laneAreaRef}
        className="flex-1 overflow-hidden select-none"
        style={{ cursor: 'grab' }}
        onWheel={handleWheel}
        onMouseDown={handleMouseDown}
      >
        <div className="relative" style={{ minHeight: lanes.length * LANE_HEIGHT + 28 }}>
          {lanes.map((lane, laneIdx) => {
            const bars = computeBarLayouts(lane.steps, lane.positionMode, sessionBounds, axisMode)
            const isFocused = viewport.focusedLaneIndex === laneIdx

            return (
              <div
                key={lane.id}
                className={cn('flex', isFocused && 'bg-neutral-800/20')}
                style={{ height: LANE_HEIGHT }}
              >
                {/* Label column */}
                <button
                  onClick={() => { setAnalyticsLaneId(analyticsLaneId === lane.id ? null : lane.id); containerRef.current?.focus() }}
                  className={cn(
                    'shrink-0 flex items-center gap-1.5 px-2 border-b border-r border-neutral-800/50',
                    'bg-neutral-900/80 sticky left-0 z-10 text-left cursor-pointer hover:bg-neutral-800/50 transition-colors',
                    isFocused && 'border-l-2 border-l-cyan-500',
                  )}
                  style={{ width: LABEL_WIDTH }}
                  title="Click for analytics"
                >
                  <span className={cn('w-2 h-2 rounded-full shrink-0', lane.color)} />
                  <span className={cn(
                    'text-[11px] truncate',
                    lane.isSubLane ? 'text-neutral-400' : 'text-neutral-200 font-medium',
                  )}>
                    {lane.isSubLane ? '↳ ' : ''}{lane.label}
                  </span>
                  {lane.steps.some(s => s.status === 'error') && (
                    <span className="w-1.5 h-1.5 rounded-full bg-red-500 shrink-0 ml-auto" />
                  )}
                </button>

                {/* Bar area */}
                <div className="flex-1 relative border-b border-neutral-800/30 bg-neutral-950/30 overflow-hidden">
                  {bars.map(({ step, leftPct, widthPct }) => {
                    const barStart = leftPct
                    const barEnd = leftPct + widthPct
                    const viewStartPct = (viewStart / totalRange) * 100
                    const viewEndPct = (viewEnd / totalRange) * 100
                    if (barEnd < viewStartPct || barStart > viewEndPct) return null

                    const viewLeft = toViewPct(barStart * totalRange / 100)
                    const viewWidth = (widthPct / visibleRange * totalRange / 100) * 100
                    const isSelected = step.id === selectedStepId
                    const isError = step.status === 'error'

                    return (
                      <div
                        key={step.id}
                        className="group absolute top-1 bottom-1"
                        style={{
                          left: `${viewLeft}%`,
                          width: `${viewWidth}%`,
                          minWidth: 4,
                        }}
                      >
                        <button
                          onClick={() => handleBarClick(step.id)}
                          aria-label={`Step ${step.step_number}: ${step.tool_name || step.step_type_label}`}
                          title={[
                            step.tool_name || step.step_type_label,
                            step.model,
                            formatDuration(step.duration_ms),
                            step.tokens_in + step.tokens_out > 0
                              ? `${formatTokens(step.tokens_in + step.tokens_out)} tok`
                              : null,
                            step.error ? `Error: ${step.error}` : null,
                          ].filter(Boolean).join(' · ')}
                          className={cn(
                            'absolute inset-0 rounded-sm transition-colors',
                            'flex items-center gap-0.5 overflow-hidden px-0.5',
                            isSelected
                              ? 'ring-2 ring-cyan-400 brightness-125 z-20'
                              : 'hover:brightness-110 z-10',
                            isError ? 'ring-1 ring-red-500/60' : '',
                            lane.color,
                            isError ? 'opacity-80' : 'opacity-70',
                            isSelected && 'opacity-100',
                          )}
                        >
                          {viewWidth > 3 && <StepTypeIcon type={step.step_type} />}
                          {viewWidth > 8 && (
                            <span className="text-[9px] text-white/80 truncate">
                              {step.tool_name || step.step_type_label}
                            </span>
                          )}
                        </button>
                        {(onFork || onReplay) && (
                          // Overlay sits INSIDE the bar wrapper (within lane bounds) so it
                          // isn't clipped by `overflow-hidden` on the bar-area and swim-lane
                          // containers. `group-focus-within` reveals it for keyboard users.
                          <div className="hidden group-hover:flex group-focus-within:flex absolute top-0 right-0 z-30 gap-0.5">
                            {onFork && (
                              <button
                                onClick={() => onFork(step)}
                                aria-label={`Fork from step ${step.step_number}`}
                                title={`Fork from step ${step.step_number}`}
                                className="flex items-center gap-0.5 px-1 py-0.5 rounded text-[9px] bg-amber-950/95 text-amber-300 border border-amber-800/60 hover:bg-amber-900/95 focus:outline-none focus:ring-1 focus:ring-amber-500 transition-colors"
                              >
                                <GitBranch size={9} />
                              </button>
                            )}
                            {onReplay && (
                              <button
                                onClick={() => onReplay(step)}
                                aria-label={`Set up replay from step ${step.step_number}`}
                                title={`Set up replay from step ${step.step_number}`}
                                className="flex items-center gap-0.5 px-1 py-0.5 rounded text-[9px] bg-cyan-950/95 text-cyan-300 border border-cyan-800/60 hover:bg-cyan-900/95 focus:outline-none focus:ring-1 focus:ring-cyan-500 transition-colors"
                              >
                                <Play size={9} />
                              </button>
                            )}
                          </div>
                        )}
                      </div>
                    )
                  })}
                </div>
              </div>
            )
          })}

          {/* Time axis */}
          <div
            className="flex items-center border-t border-neutral-800/50"
            style={{ paddingLeft: LABEL_WIDTH, height: 24 }}
          >
            <TimeAxis
              positionMode={lanes[0]?.positionMode ?? 'created_at'}
              bounds={sessionBounds}
              viewStart={viewStart}
              viewEnd={viewEnd}
              zoom={viewport.zoom}
            />
          </div>
        </div>
      </div>
    </div>
  )
}

// --- Lane Analytics Popover ---

export function computeLaneAnalytics(steps: StepResponse[]) {
  const total = steps.length
  if (total === 0) return null

  const totalDuration = steps.reduce((s, st) => s + st.duration_ms, 0)
  const totalTokens = steps.reduce((s, st) => s + st.tokens_in + st.tokens_out, 0)
  const errors = steps.filter(s => s.status === 'error').length

  const typeCounts: Record<string, number> = {}
  const toolCounts: Record<string, number> = {}
  for (const step of steps) {
    typeCounts[step.step_type] = (typeCounts[step.step_type] || 0) + 1
    if (step.tool_name) toolCounts[step.tool_name] = (toolCounts[step.tool_name] || 0) + 1
  }

  const topTools = Object.entries(toolCounts).sort((a, b) => b[1] - a[1]).slice(0, 5)

  return { total, totalDuration, totalTokens, errors, typeCounts, topTools }
}

function LaneAnalytics({ lane, onClose }: { lane: Lane; onClose: () => void }) {
  const analytics = computeLaneAnalytics(lane.steps)
  if (!analytics) return null

  return (
    <div className="border-b border-neutral-800 bg-neutral-900/90 px-3 py-2 shrink-0">
      <div className="flex items-center justify-between mb-1.5">
        <span className="text-[11px] font-medium text-neutral-200">{lane.label}</span>
        <button onClick={onClose} className="text-[10px] text-neutral-500 hover:text-neutral-300">✕</button>
      </div>
      <div className="grid grid-cols-4 gap-3 text-[10px]">
        <div>
          <span className="text-neutral-500">Steps</span>
          <div className="text-neutral-200 font-medium">{analytics.total}</div>
        </div>
        <div>
          <span className="text-neutral-500">Duration</span>
          <div className="text-amber-400 font-medium">{formatDuration(analytics.totalDuration)}</div>
        </div>
        <div>
          <span className="text-neutral-500">Tokens</span>
          <div className="text-blue-400 font-medium">{formatTokens(analytics.totalTokens)}</div>
        </div>
        <div>
          <span className="text-neutral-500">Errors</span>
          <div className={cn('font-medium', analytics.errors > 0 ? 'text-red-400' : 'text-green-400')}>
            {analytics.errors > 0 ? analytics.errors : 'None'}
          </div>
        </div>
      </div>
      {analytics.topTools.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {analytics.topTools.map(([name, count]) => (
            <span key={name} className="text-[9px] bg-neutral-800 text-neutral-400 px-1.5 py-0.5 rounded">
              {name} ×{count}
            </span>
          ))}
        </div>
      )}
      {Object.keys(analytics.typeCounts).length > 1 && (
        <div className="mt-1.5 flex gap-1">
          {Object.entries(analytics.typeCounts).map(([type, count]) => {
            const maxCount = Object.values(analytics.typeCounts).reduce((a, b) => Math.max(a, b), 0)
            return (
              <div key={type} className="flex-1">
                <div className="h-1.5 rounded-full bg-neutral-800 overflow-hidden">
                  <div className="h-full bg-neutral-500 rounded-full" style={{ width: `${(count / maxCount) * 100}%` }} />
                </div>
                <span className="text-[8px] text-neutral-600">{type.replace('_', ' ')} ({count})</span>
              </div>
            )
          })}
        </div>
      )}
    </div>
  )
}

// --- Minimap ---

function Minimap({
  lanes,
  minimapBars,
  totalRange,
  viewport,
  onSetOffset,
}: {
  lanes: Lane[]
  minimapBars: { lane: Lane; bars: BarLayout[] }[]
  totalRange: number
  viewport: ViewportState
  onSetOffset: (offset: number) => void
}) {
  const minimapRef = useRef<HTMLDivElement>(null)
  const draggingMinimap = useRef(false)

  const viewportFraction = 1 / viewport.zoom
  const viewportLeftFraction = totalRange > 0 ? viewport.offset / totalRange : 0

  const handleMinimapMouseDown = useCallback((e: React.MouseEvent) => {
    if (e.button !== 0) return
    draggingMinimap.current = true
    const rect = minimapRef.current?.getBoundingClientRect()
    if (!rect) return
    const barArea = rect.width - LABEL_WIDTH
    const clickFraction = (e.clientX - rect.left - LABEL_WIDTH) / barArea
    const newOffset = Math.max(0, (clickFraction - viewportFraction / 2) * totalRange)
    onSetOffset(newOffset)
    e.preventDefault()
  }, [totalRange, viewportFraction, onSetOffset])

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!draggingMinimap.current || !minimapRef.current) return
      const rect = minimapRef.current.getBoundingClientRect()
      const barArea = rect.width - LABEL_WIDTH
      const clickFraction = (e.clientX - rect.left - LABEL_WIDTH) / barArea
      const newOffset = Math.max(0, (clickFraction - viewportFraction / 2) * totalRange)
      onSetOffset(newOffset)
    }
    const onUp = () => { draggingMinimap.current = false }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
  }, [totalRange, viewportFraction, onSetOffset])

  if (viewport.zoom <= 1) return null

  return (
    <div
      ref={minimapRef}
      className="border-b border-neutral-800/50 bg-neutral-950/50 cursor-pointer shrink-0 select-none"
      style={{ height: 36 }}
      onMouseDown={handleMinimapMouseDown}
    >
      <div className="flex h-full">
        <div className="shrink-0 flex items-center px-2 bg-neutral-900/80 border-r border-neutral-800/50" style={{ width: LABEL_WIDTH }}>
          <span className="text-[9px] text-neutral-600 uppercase tracking-wider">Overview</span>
        </div>
        <div className="flex-1 relative">
          {minimapBars.map(({ lane, bars }, laneIdx) => {
            const laneH = Math.max(2, (36 - 4) / lanes.length)
            const laneTop = laneIdx * laneH + 2
            return bars.map(({ step, leftPct, widthPct }) => (
              <div
                key={step.id}
                className={cn('absolute rounded-[1px]', lane.color, 'opacity-40')}
                style={{
                  left: `${leftPct}%`,
                  width: `${widthPct}%`,
                  minWidth: 1,
                  top: laneTop,
                  height: Math.max(2, laneH - 1),
                }}
              />
            ))
          })}
          <div
            className="absolute top-0 bottom-0 border border-cyan-500/60 bg-cyan-500/10 rounded-sm pointer-events-none"
            style={{
              left: `${viewportLeftFraction * 100}%`,
              width: `${viewportFraction * 100}%`,
            }}
          />
        </div>
      </div>
    </div>
  )
}

// --- Time axis ---

function TimeAxis({
  positionMode,
  bounds,
  viewStart,
  viewEnd,
  zoom,
}: {
  positionMode: 'created_at' | 'step_number'
  bounds: { startMs: number; endMs: number; maxStep: number }
  viewStart: number
  viewEnd: number
  zoom: number
}) {
  const ticks = useMemo(() => {
    const count = Math.max(4, Math.min(12, Math.floor(6 * zoom)))
    const result: { pct: number; label: string }[] = []
    const visibleRange = viewEnd - viewStart
    if (visibleRange <= 0) return [{ pct: 0, label: '0s' }]

    if (positionMode === 'step_number') {
      const step = Math.max(1, Math.ceil(visibleRange / count))
      const start = Math.floor(viewStart / step) * step
      for (let i = start; i <= viewEnd; i += step) {
        const pct = ((i - viewStart) / visibleRange) * 100
        if (pct >= -5 && pct <= 105) {
          result.push({ pct, label: `#${Math.round(i) + 1}` })
        }
      }
    } else {
      const stepMs = visibleRange / count
      for (let i = 0; i <= count; i++) {
        const ms = viewStart + i * stepMs
        const pct = (i / count) * 100
        result.push({ pct, label: formatDuration(Math.round(ms)) })
      }
    }

    return result
  }, [positionMode, viewStart, viewEnd, zoom])

  return (
    <div className="relative w-full h-full">
      {ticks.map(({ pct, label }, i) => (
        <span
          key={i}
          className="absolute text-[9px] text-neutral-600 -translate-x-1/2"
          style={{ left: `${pct}%`, top: 4 }}
        >
          {label}
        </span>
      ))}
    </div>
  )
}
