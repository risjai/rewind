import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, cleanup } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { StepDetailPanel } from './StepDetailPanel'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import type { StepDetail } from '@/types/api'

// Stub the API so the test doesn't try to network. The panel gates on
// `isLoading` and `step` from `useQuery`; returning a Promise.resolve(...)
// runs the success branch synchronously enough for the assertions.
vi.mock('@/lib/api', () => ({
  api: {
    stepDetail: vi.fn(),
    health: vi.fn().mockResolvedValue({ status: 'ok', version: '0.14.4', allow_main_edits: false }),
    // Session mock includes BOTH a root timeline AND a fork. Existing
    // tests that don't set selectedTimelineId still get fallback=root
    // (=tl-main), so their data-tid assertions keep passing. The new
    // timeline-context tests set selectedTimelineId='tl-fork' to drive
    // the context-routing branch.
    session: vi.fn().mockResolvedValue({
      session: { id: 'sess-1', name: 'test', created_at: '', updated_at: '', status: 'Completed', total_steps: 4, total_tokens: 0, metadata: {} },
      timelines: [
        { id: 'tl-main', session_id: 'sess-1', parent_timeline_id: null, fork_at_step: null, created_at: '', label: 'main' },
        { id: 'tl-fork', session_id: 'sess-1', parent_timeline_id: 'tl-main', fork_at_step: 2, created_at: '', label: 'fork' },
      ],
    }),
    cascadeCount: vi.fn().mockResolvedValue({ deleted_downstream_count: 2, on_main: true }),
    patchStep: vi.fn().mockResolvedValue({ step_id: 'step-abc', deleted_downstream_count: 2 }),
    forkAndEditStep: vi.fn().mockResolvedValue({ fork_timeline_id: 'tl-fork-1', step_id: 'step-new' }),
  },
}))

// The two modal components mount as siblings; we don't care what they
// render — just that the right one becomes visible when its button
// fires. Stubbing them out keeps the test focused on header buttons.
// All three stubs capture `timelineId`/`sourceTimelineId` as data-tid
// so timeline-context routing tests can assert which timeline the
// dashboard passed in.
vi.mock('./ForkModal', () => ({
  ForkModal: ({ isOpen, sessionId, timelineId, atStep }: { isOpen: boolean; sessionId: string; timelineId: string; atStep: number }) =>
    isOpen ? (
      <div role="dialog" aria-label="ForkModal-stub" data-sid={sessionId} data-tid={timelineId} data-step={atStep} />
    ) : null,
}))
vi.mock('./ReplaySetupModal', () => ({
  ReplaySetupModal: ({ isOpen, sessionId, timelineId, atStep }: { isOpen: boolean; sessionId: string; timelineId: string; atStep: number }) =>
    isOpen ? (
      <div role="dialog" aria-label="ReplaySetupModal-stub" data-sid={sessionId} data-tid={timelineId} data-step={atStep} />
    ) : null,
}))
vi.mock('./RunReplayButton', () => ({
  ReplayJobModal: ({
    sessionId,
    sourceTimelineId,
    atStep,
  }: {
    sessionId: string
    sourceTimelineId: string
    atStep: number
  }) => (
    <div
      role="dialog"
      aria-label="ReplayJobModal-stub"
      data-sid={sessionId}
      data-tid={sourceTimelineId}
      data-step={atStep}
    />
  ),
}))

function renderWithClient(ui: React.ReactElement) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>)
}

function makeStep(overrides: Partial<StepDetail> = {}): StepDetail {
  return {
    id: 'step-abc',
    session_id: 'sess-1',
    timeline_id: 'tl-main',
    step_number: 4,
    step_type: 'tool_call',
    status: 'success',
    duration_ms: 171,
    tokens_in: 0,
    tokens_out: 0,
    model: '',
    tool_name: 'get_cluster_pods',
    error: null,
    request_body: null,
    response_body: null,
    messages: null,
    ...overrides,
  } as StepDetail
}

describe('StepDetailPanel — Run replay button', () => {
  beforeEach(() => {
    cleanup()
    vi.mocked(api.stepDetail).mockResolvedValue(makeStep())
  })

  it('renders all three header action buttons (Fork / Set up replay / Run replay)', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    expect(await screen.findByRole('button', { name: /fork from here/i })).toBeTruthy()
    expect(screen.getByRole('button', { name: /set up replay/i })).toBeTruthy()
    expect(screen.getByRole('button', { name: /run replay/i })).toBeTruthy()
  })

  it('opens ReplayJobModal when Run replay is clicked, with the right session + timeline + step', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    const runReplayBtn = await screen.findByRole('button', { name: /run replay/i })
    fireEvent.click(runReplayBtn)
    const modal = await screen.findByRole('dialog', { name: 'ReplayJobModal-stub' })
    expect(modal.getAttribute('data-sid')).toBe('sess-1')
    expect(modal.getAttribute('data-tid')).toBe('tl-main')
    expect(modal.getAttribute('data-step')).toBe('4')
  })

  it('does NOT open the legacy ReplaySetupModal when Run replay is clicked', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByRole('button', { name: /run replay/i }))
    expect(screen.queryByRole('dialog', { name: 'ReplaySetupModal-stub' })).toBeNull()
  })

  it('Set up replay opens the legacy modal, not the Phase 3 one', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByRole('button', { name: /set up replay/i }))
    expect(screen.getByRole('dialog', { name: 'ReplaySetupModal-stub' })).toBeTruthy()
    expect(screen.queryByRole('dialog', { name: 'ReplayJobModal-stub' })).toBeNull()
  })

  it('Fork from here opens the fork modal, not the replay ones', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByRole('button', { name: /fork from here/i }))
    expect(screen.getByRole('dialog', { name: 'ForkModal-stub' })).toBeTruthy()
    expect(screen.queryByRole('dialog', { name: 'ReplayJobModal-stub' })).toBeNull()
    expect(screen.queryByRole('dialog', { name: 'ReplaySetupModal-stub' })).toBeNull()
  })

  it('only one modal is open at a time when switching between buttons', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByRole('button', { name: /fork from here/i }))
    expect(screen.getAllByRole('dialog').length).toBe(1)
    fireEvent.click(screen.getByRole('button', { name: /run replay/i }))
    // Open state is per-modal, so opening one without closing the other
    // could in principle render two; this asserts the implementation
    // tracks a single `modalMode` state.
    expect(screen.getAllByRole('dialog').length).toBe(1)
    expect(screen.getByRole('dialog').getAttribute('aria-label')).toBe('ReplayJobModal-stub')
  })
})

describe('StepDetailPanel — Step editing', () => {
  beforeEach(() => {
    cleanup()
    vi.mocked(api.stepDetail).mockResolvedValue(
      makeStep({
        request_body: { model: 'gpt-4o', messages: [{ role: 'user', content: 'hello' }] },
        response_body: { choices: [{ message: { content: 'world' } }] },
      }),
    )
  })

  it('shows Edit pencil button on active Request tab when data exists', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    expect(await screen.findByTitle(/edit request/i)).toBeTruthy()
  })

  it('opens editor with initial JSON on Edit click and Cancel returns to view', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByTitle(/edit request/i))
    expect(screen.getByRole('textbox')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: /cancel/i }))
    expect(screen.queryByRole('textbox')).toBeNull()
  })

  it('disables Save when JSON is invalid', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByTitle(/edit request/i))
    const textarea = screen.getByRole('textbox')
    fireEvent.change(textarea, { target: { value: '{ invalid json' } })
    const saveBtn = screen.getByRole('button', { name: /^save$/i })
    expect(saveBtn.hasAttribute('disabled')).toBe(true)
  })

  it('disables Save when text has not changed', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByTitle(/edit request/i))
    const saveBtn = screen.getByRole('button', { name: /^save$/i })
    expect(saveBtn.hasAttribute('disabled')).toBe(true)
  })

  it('enables Save when JSON is valid and text has changed', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByTitle(/edit request/i))
    const textarea = screen.getByRole('textbox')
    fireEvent.change(textarea, { target: { value: '{"model":"gpt-4o-mini"}' } })
    const saveBtn = screen.getByRole('button', { name: /^save$/i })
    expect(saveBtn.hasAttribute('disabled')).toBe(false)
  })

  it('shows Edit pencil for Response tab too', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    const respTab = await screen.findByText('Response')
    fireEvent.click(respTab)
    expect(screen.getByTitle(/edit response/i)).toBeTruthy()
  })
})

// ──────────────────────────────────────────────────────────────────
// Timeline-context routing for Fork / Set up replay / Run replay
//
// Bug class fixed in this PR (and previously in #161 for the Edit
// button): all four step-action buttons must source their action
// from the user's SELECTED timeline, not the step's physical owner.
// Otherwise inherited steps shown on a fork dispatch against the
// parent (often main), discarding the user's fork lineage.
//
// `contextTimelineId` fallback chain in StepDetailPanel:
//   selectedTimelineId  ??  rootTimelineId  ??  step.timeline_id
//
// Each modal stub reflects the prop it was given as `data-tid`.
// ──────────────────────────────────────────────────────────────────

describe('StepDetailPanel — timeline-context routing for action buttons', () => {
  beforeEach(() => {
    cleanup()
    vi.mocked(api.stepDetail).mockResolvedValue(makeStep())
    // Drive the SELECTED-timeline branch: user is viewing tl-fork
    // even though the step physically lives on tl-main (i.e. it's
    // an inherited step).
    useStore.setState({ selectedTimelineId: 'tl-fork' })
  })

  afterEach(() => {
    // Reset so other test suites that rely on null get a clean slate.
    useStore.setState({ selectedTimelineId: null })
  })

  it('ForkModal receives the SELECTED timeline (not step.timeline_id) when they differ', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByRole('button', { name: /fork from here/i }))
    const modal = await screen.findByRole('dialog', { name: 'ForkModal-stub' })
    expect(modal.getAttribute('data-tid')).toBe('tl-fork')
    expect(modal.getAttribute('data-tid')).not.toBe('tl-main')
  })

  it('ReplaySetupModal receives the SELECTED timeline (not step.timeline_id) when they differ', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByRole('button', { name: /set up replay/i }))
    const modal = await screen.findByRole('dialog', { name: 'ReplaySetupModal-stub' })
    expect(modal.getAttribute('data-tid')).toBe('tl-fork')
    expect(modal.getAttribute('data-tid')).not.toBe('tl-main')
  })

  it('ReplayJobModal receives the SELECTED timeline (not step.timeline_id) when they differ', async () => {
    renderWithClient(<StepDetailPanel stepId="step-abc" />)
    fireEvent.click(await screen.findByRole('button', { name: /run replay/i }))
    const modal = await screen.findByRole('dialog', { name: 'ReplayJobModal-stub' })
    // Pre-fix this would have been 'tl-main' (step.timeline_id),
    // dispatching the replay against main instead of the user's
    // fork — the original dev1 repro on session ray-agent-85551571.
    expect(modal.getAttribute('data-tid')).toBe('tl-fork')
    expect(modal.getAttribute('data-tid')).not.toBe('tl-main')
  })
})
