import { describe, it, expect, vi } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import { buildLanes, viewportReducer, computeLaneAnalytics, computeBarLayouts, ActivityTimeline, type Lane, type ViewportState } from './ActivityTimeline'
import type { SpanResponse, StepResponse, Session } from '@/types/api'

function makeStep(overrides: Partial<StepResponse> = {}): StepResponse {
  return {
    id: 'step-1',
    timeline_id: 'tl-1',
    session_id: 's-1',
    step_number: 1,
    step_type: 'tool_call',
    step_type_label: 'Tool Call',
    step_type_icon: '🔧',
    status: 'success',
    created_at: '2026-04-14T10:00:00Z',
    duration_ms: 100,
    tokens_in: 0,
    tokens_out: 0,
    model: '',
    error: null,
    response_preview: '',
    ...overrides,
  }
}

function makeSpan(overrides: Partial<SpanResponse> = {}): SpanResponse {
  return {
    id: 'span-1',
    session_id: 's-1',
    timeline_id: 'tl-1',
    parent_span_id: null,
    span_type: 'agent',
    span_type_icon: '🤖',
    name: 'orchestrator',
    status: 'completed',
    started_at: '2026-04-14T10:00:00Z',
    ended_at: '2026-04-14T10:00:05Z',
    duration_ms: 5000,
    metadata: {},
    error: null,
    child_spans: [],
    steps: [],
    ...overrides,
  }
}

function makeSession(overrides: Partial<Session> = {}): Session {
  return {
    id: 's-1',
    name: 'test',
    created_at: '2026-04-14T10:00:00Z',
    updated_at: '2026-04-14T10:00:00Z',
    status: 'Completed',
    total_steps: 0,
    total_tokens: 0,
    metadata: {},
    source: 'proxy',
    ...overrides,
  }
}

describe('buildLanes', () => {
  describe('Mode A: sessions with agent spans', () => {
    it('creates one lane per agent span', () => {
      const spans: SpanResponse[] = [
        makeSpan({
          id: 'agent-1',
          name: 'supervisor',
          steps: [makeStep({ id: 's1', step_number: 1 })],
          child_spans: [
            makeSpan({
              id: 'agent-2',
              name: 'researcher',
              span_type: 'agent',
              parent_span_id: 'agent-1',
              steps: [makeStep({ id: 's2', step_number: 2 }), makeStep({ id: 's3', step_number: 3 })],
            }),
          ],
        }),
      ]

      const lanes = buildLanes(spans, [], makeSession())
      expect(lanes).toHaveLength(2)
      expect(lanes[0].label).toBe('supervisor')
      expect(lanes[0].isSubLane).toBe(false)
      expect(lanes[0].steps).toHaveLength(1)
      expect(lanes[1].label).toBe('researcher')
      expect(lanes[1].isSubLane).toBe(true)
      expect(lanes[1].steps).toHaveLength(2)
    })

    it('handles root-level non-agent spans from OTel imports', () => {
      const spans: SpanResponse[] = [
        makeSpan({
          id: 'otel-root',
          name: 'http-server',
          span_type: 'http',
          steps: [makeStep({ id: 's1' })],
        }),
      ]

      const lanes = buildLanes(spans, [], makeSession())
      expect(lanes).toHaveLength(1)
      expect(lanes[0].label).toBe('http-server')
    })
  })

  describe('Mode B: hook sessions without spans', () => {
    it('groups steps by tool_name into sub-lanes', () => {
      const steps: StepResponse[] = [
        makeStep({ id: 's1', step_number: 1, step_type: 'tool_call', tool_name: 'Read' }),
        makeStep({ id: 's2', step_number: 2, step_type: 'tool_call', tool_name: 'Write' }),
        makeStep({ id: 's3', step_number: 3, step_type: 'tool_call', tool_name: 'Read' }),
        makeStep({ id: 's4', step_number: 4, step_type: 'llm_call', tool_name: undefined }),
        makeStep({ id: 's5', step_number: 5, step_type: 'user_prompt', tool_name: undefined }),
      ]

      const lanes = buildLanes([], steps, makeSession({ source: 'hooks' }))
      const labels = lanes.map(l => l.label)
      expect(labels).toContain('LLM Calls')
      expect(labels).toContain('Read')
      expect(labels).toContain('Write')
      expect(labels).toContain('Prompts')

      const readLane = lanes.find(l => l.label === 'Read')!
      expect(readLane.steps).toHaveLength(2)
    })

    it('uses step_number positioning for hook sessions', () => {
      const steps: StepResponse[] = [
        makeStep({ id: 's1', step_number: 1, step_type: 'tool_call', tool_name: 'Read' }),
        makeStep({ id: 's2', step_number: 2, step_type: 'tool_call', tool_name: 'Read' }),
      ]

      const lanes = buildLanes([], steps, makeSession({ source: 'hooks' }))
      expect(lanes[0].positionMode).toBe('step_number')
    })

    it('uses created_at positioning for proxy sessions', () => {
      const spans: SpanResponse[] = [
        makeSpan({ steps: [makeStep()] }),
      ]

      const lanes = buildLanes(spans, [], makeSession({ source: 'proxy' }))
      expect(lanes[0].positionMode).toBe('created_at')
    })
  })

  describe('edge cases', () => {
    it('returns empty array for no steps and no spans', () => {
      const lanes = buildLanes([], [], makeSession())
      expect(lanes).toHaveLength(0)
    })

    it('collects steps without a span into a fallback lane', () => {
      const spans: SpanResponse[] = [
        makeSpan({
          id: 'agent-1',
          name: 'main',
          steps: [],
          child_spans: [],
        }),
      ]
      const steps: StepResponse[] = [
        makeStep({ id: 's1', step_number: 1 }),
      ]

      const lanes = buildLanes(spans, steps, makeSession())
      const hasSteps = lanes.some(l => l.steps.length > 0)
      expect(hasSteps).toBe(true)
    })
  })
})

describe('viewportReducer', () => {
  const initial: ViewportState = { zoom: 1, offset: 0, focusedLaneIndex: null }

  it('zooms in, clamped to max 50', () => {
    const state = viewportReducer(initial, { type: 'zoom_in' })
    expect(state.zoom).toBeGreaterThan(1)

    let s = { ...initial, zoom: 50 }
    s = viewportReducer(s, { type: 'zoom_in' })
    expect(s.zoom).toBe(50)
  })

  it('zooms out, clamped to min 1', () => {
    const state = viewportReducer({ ...initial, zoom: 5 }, { type: 'zoom_out' })
    expect(state.zoom).toBeLessThan(5)

    let s = { ...initial, zoom: 1 }
    s = viewportReducer(s, { type: 'zoom_out' })
    expect(s.zoom).toBe(1)
  })

  it('resets zoom to 1', () => {
    const state = viewportReducer({ ...initial, zoom: 10, offset: 500 }, { type: 'reset' })
    expect(state.zoom).toBe(1)
    expect(state.offset).toBe(0)
  })

  it('pans by delta', () => {
    const state = viewportReducer(initial, { type: 'pan', delta: 100 })
    expect(state.offset).toBe(100)
  })

  it('sets offset directly', () => {
    const state = viewportReducer(initial, { type: 'set_offset', offset: 250 })
    expect(state.offset).toBe(250)
  })

  it('focuses a lane by index', () => {
    const state = viewportReducer(initial, { type: 'focus_lane', index: 2 })
    expect(state.focusedLaneIndex).toBe(2)
  })

  it('handles wheel zoom at a cursor position', () => {
    const state = viewportReducer(
      { zoom: 2, offset: 100, focusedLaneIndex: null },
      { type: 'wheel_zoom', deltaY: -100, cursorFraction: 0.5, totalRange: 10000 }
    )
    expect(state.zoom).toBeGreaterThan(2)
  })

  it('pan clamps offset to >= 0', () => {
    const state = viewportReducer(initial, { type: 'pan', delta: -500 })
    expect(state.offset).toBe(0)
  })
})

describe('computeBarLayouts', () => {
  const bounds = { startMs: 1000, endMs: 5000, maxStep: 5 }

  it('computes positions for step_number mode', () => {
    const steps = [
      makeStep({ id: 'a', step_number: 1, duration_ms: 500 }),
      makeStep({ id: 'b', step_number: 3, duration_ms: 200 }),
    ]
    const bars = computeBarLayouts(steps, 'step_number', bounds)
    expect(bars).toHaveLength(2)
    expect(bars[0].leftPct).toBe(0)
    expect(bars[1].leftPct).toBeCloseTo(40)
    expect(bars[0].widthPct).toBeGreaterThan(0)
  })

  it('computes positions for created_at mode', () => {
    const steps = [
      makeStep({ id: 'a', step_number: 1, created_at: '2026-04-14T10:00:01Z', duration_ms: 1000 }),
      makeStep({ id: 'b', step_number: 2, created_at: '2026-04-14T10:00:03Z', duration_ms: 500 }),
    ]
    const b = { startMs: new Date('2026-04-14T10:00:01Z').getTime(), endMs: new Date('2026-04-14T10:00:03.5Z').getTime(), maxStep: 2 }
    const bars = computeBarLayouts(steps, 'created_at', b)
    expect(bars).toHaveLength(2)
    expect(bars[0].leftPct).toBeCloseTo(0)
    expect(bars[1].leftPct).toBeGreaterThan(0)
  })

  it('returns empty array for empty steps', () => {
    expect(computeBarLayouts([], 'created_at', bounds)).toHaveLength(0)
  })

  it('handles zero totalMs with even spacing', () => {
    const steps = [
      makeStep({ id: 'a', step_number: 1, created_at: '2026-04-14T10:00:00Z', duration_ms: 0 }),
      makeStep({ id: 'b', step_number: 2, created_at: '2026-04-14T10:00:00Z', duration_ms: 0 }),
    ]
    const b = { startMs: new Date('2026-04-14T10:00:00Z').getTime(), endMs: new Date('2026-04-14T10:00:00Z').getTime(), maxStep: 2 }
    const bars = computeBarLayouts(steps, 'created_at', b)
    expect(bars).toHaveLength(2)
    expect(bars[0].leftPct).toBe(0)
    expect(bars[1].leftPct).toBe(90)
    expect(bars[0].widthPct).toBeGreaterThan(0)
  })

  it('handles tokens axis mode', () => {
    const steps = [makeStep({ id: 'a', step_number: 1, duration_ms: 100, tokens_in: 500, tokens_out: 200 })]
    const bars = computeBarLayouts(steps, 'step_number', bounds, 'tokens')
    expect(bars).toHaveLength(1)
    expect(bars[0].widthPct).toBeGreaterThan(0)
  })
})

describe('ActivityTimeline keyboard navigation', () => {
  const threeSteps: StepResponse[] = [
    makeStep({ id: 'a', step_number: 1, created_at: '2026-04-14T10:00:00Z', duration_ms: 1000 }),
    makeStep({ id: 'b', step_number: 2, created_at: '2026-04-14T10:00:01Z', duration_ms: 2000 }),
    makeStep({ id: 'c', step_number: 3, created_at: '2026-04-14T10:00:03Z', duration_ms: 500 }),
  ]
  const spanWithSteps = makeSpan({
    id: 'agent-1',
    name: 'main-agent',
    steps: threeSteps,
  })
  const session = makeSession()

  function renderTimeline(selectedStepId: string | null = null) {
    const onSelectStep = vi.fn()
    const result = render(
      <ActivityTimeline
        spans={[spanWithSteps]}
        steps={threeSteps}
        session={session}
        selectedStepId={selectedStepId}
        onSelectStep={onSelectStep}
      />
    )
    // RTL's result.container is a wrapper div; our component root with tabIndex is inside it
    const timeline = result.container.querySelector('[tabindex="0"]') as HTMLElement
    return { onSelectStep, timeline, ...result }
  }

  it('j selects the first step when nothing is selected', () => {
    const { onSelectStep, timeline } = renderTimeline(null)
    expect(timeline).toBeTruthy()
    fireEvent.keyDown(timeline, { key: 'j' })
    expect(onSelectStep).toHaveBeenCalledWith('a')
  })

  it('j selects the next step when a step is already selected', () => {
    const { onSelectStep, timeline } = renderTimeline('a')
    fireEvent.keyDown(timeline, { key: 'j' })
    expect(onSelectStep).toHaveBeenCalledWith('b')
  })

  it('k selects the previous step', () => {
    const { onSelectStep, timeline } = renderTimeline('b')
    fireEvent.keyDown(timeline, { key: 'k' })
    expect(onSelectStep).toHaveBeenCalledWith('a')
  })

  it('k does nothing when on the first step', () => {
    const { onSelectStep, timeline } = renderTimeline('a')
    fireEvent.keyDown(timeline, { key: 'k' })
    expect(onSelectStep).not.toHaveBeenCalled()
  })

  it('j does nothing when on the last step', () => {
    const { onSelectStep, timeline } = renderTimeline('c')
    fireEvent.keyDown(timeline, { key: 'j' })
    expect(onSelectStep).not.toHaveBeenCalled()
  })

  it('Escape deselects the current step', () => {
    const { onSelectStep, timeline } = renderTimeline('b')
    fireEvent.keyDown(timeline, { key: 'Escape' })
    expect(onSelectStep).toHaveBeenCalledWith(null)
  })

  it('ArrowDown works as alternative to j', () => {
    const { onSelectStep, timeline } = renderTimeline(null)
    fireEvent.keyDown(timeline, { key: 'ArrowDown' })
    expect(onSelectStep).toHaveBeenCalledWith('a')
  })

  it('ArrowUp works as alternative to k', () => {
    const { onSelectStep, timeline } = renderTimeline('b')
    fireEvent.keyDown(timeline, { key: 'ArrowUp' })
    expect(onSelectStep).toHaveBeenCalledWith('a')
  })

  it('zoom keys do not call onSelectStep', () => {
    const { onSelectStep, timeline } = renderTimeline('a')
    fireEvent.keyDown(timeline, { key: '+' })
    fireEvent.keyDown(timeline, { key: '-' })
    fireEvent.keyDown(timeline, { key: '0' })
    expect(onSelectStep).not.toHaveBeenCalled()
  })

  it('renders zoom indicator when zoom > 1 after pressing +', () => {
    const { timeline } = renderTimeline(null)
    fireEvent.keyDown(timeline, { key: '+' })
    expect(screen.getByText(/Reset/)).toBeTruthy()
  })
})

describe('computeLaneAnalytics', () => {
  it('computes correct aggregate metrics', () => {
    const steps: StepResponse[] = [
      makeStep({ id: 'a', duration_ms: 100, tokens_in: 50, tokens_out: 25, status: 'success', tool_name: 'Read' }),
      makeStep({ id: 'b', duration_ms: 200, tokens_in: 100, tokens_out: 50, status: 'error', tool_name: 'Write' }),
      makeStep({ id: 'c', duration_ms: 300, tokens_in: 75, tokens_out: 30, status: 'success', tool_name: 'Read' }),
    ]
    const result = computeLaneAnalytics(steps)!
    expect(result.total).toBe(3)
    expect(result.totalDuration).toBe(600)
    expect(result.totalTokens).toBe(330)
    expect(result.errors).toBe(1)
    expect(result.topTools).toEqual([['Read', 2], ['Write', 1]])
  })

  it('returns null for empty steps', () => {
    expect(computeLaneAnalytics([])).toBeNull()
  })
})

describe('ActivityTimeline axis mode', () => {
  const threeSteps: StepResponse[] = [
    makeStep({ id: 'a', step_number: 1, created_at: '2026-04-14T10:00:00Z', duration_ms: 1000, tokens_in: 100, tokens_out: 50 }),
    makeStep({ id: 'b', step_number: 2, created_at: '2026-04-14T10:00:01Z', duration_ms: 2000, tokens_in: 200, tokens_out: 100 }),
    makeStep({ id: 'c', step_number: 3, created_at: '2026-04-14T10:00:03Z', duration_ms: 500, tokens_in: 50, tokens_out: 25 }),
  ]
  const spanWithSteps = makeSpan({ id: 'agent-1', name: 'main-agent', steps: threeSteps })
  const session = makeSession()

  it('renders Duration/Tokens/Cost axis selector', () => {
    render(
      <ActivityTimeline spans={[spanWithSteps]} steps={threeSteps} session={session} selectedStepId={null} onSelectStep={() => {}} />
    )
    expect(screen.getByText('duration')).toBeTruthy()
    expect(screen.getByText('tokens')).toBeTruthy()
    expect(screen.getByText('cost (est.)')).toBeTruthy()
  })

  it('disables tokens and cost for Cursor sessions', () => {
    render(
      <ActivityTimeline spans={[spanWithSteps]} steps={threeSteps} session={session} selectedStepId={null} onSelectStep={() => {}} isCursor={true} />
    )
    const tokensBtn = screen.getByText('tokens')
    expect(tokensBtn.closest('button')?.disabled).toBe(true)
    const costBtn = screen.getByText('cost (est.)')
    expect(costBtn.closest('button')?.disabled).toBe(true)
  })

  it('shows error count in header when errors exist', () => {
    const stepsWithError = [
      ...threeSteps,
      makeStep({ id: 'd', step_number: 4, status: 'error', created_at: '2026-04-14T10:00:04Z' }),
    ]
    const span = makeSpan({ id: 'agent-1', name: 'main', steps: stepsWithError })
    render(
      <ActivityTimeline spans={[span]} steps={stepsWithError} session={session} selectedStepId={null} onSelectStep={() => {}} />
    )
    expect(screen.getByText(/1 error/)).toBeTruthy()
  })

  it('renders auto-follow button when isLive', () => {
    render(
      <ActivityTimeline spans={[spanWithSteps]} steps={threeSteps} session={session} selectedStepId={null} onSelectStep={() => {}} isLive={true} />
    )
    expect(screen.getByText('Following')).toBeTruthy()
  })

  it('shows lane analytics popover on label click', () => {
    const { container } = render(
      <ActivityTimeline spans={[spanWithSteps]} steps={threeSteps} session={session} selectedStepId={null} onSelectStep={() => {}} />
    )
    const label = screen.getByText('main-agent')
    fireEvent.click(label.closest('button')!)
    expect(screen.getByText('Steps')).toBeTruthy()
    expect(screen.getByText('Duration')).toBeTruthy()
    expect(screen.getByText('Tokens')).toBeTruthy()
  })
})
