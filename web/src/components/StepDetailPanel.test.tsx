import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent, cleanup } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { StepDetailPanel } from './StepDetailPanel'
import { api } from '@/lib/api'
import type { StepDetail } from '@/types/api'

// Stub the API so the test doesn't try to network. The panel gates on
// `isLoading` and `step` from `useQuery`; returning a Promise.resolve(...)
// runs the success branch synchronously enough for the assertions.
vi.mock('@/lib/api', () => ({
  api: {
    stepDetail: vi.fn(),
  },
}))

// The two modal components mount as siblings; we don't care what they
// render — just that the right one becomes visible when its button
// fires. Stubbing them out keeps the test focused on header buttons.
vi.mock('./ForkModal', () => ({
  ForkModal: ({ isOpen, sessionId, atStep }: { isOpen: boolean; sessionId: string; atStep: number }) =>
    isOpen ? (
      <div role="dialog" aria-label="ForkModal-stub" data-sid={sessionId} data-step={atStep} />
    ) : null,
}))
vi.mock('./ReplaySetupModal', () => ({
  ReplaySetupModal: ({ isOpen, sessionId, atStep }: { isOpen: boolean; sessionId: string; atStep: number }) =>
    isOpen ? (
      <div role="dialog" aria-label="ReplaySetupModal-stub" data-sid={sessionId} data-step={atStep} />
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
