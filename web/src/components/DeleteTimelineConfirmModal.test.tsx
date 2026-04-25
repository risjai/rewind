import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { DeleteTimelineConfirmModal } from './DeleteTimelineConfirmModal'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'
import type { Timeline } from '@/types/api'

vi.mock('@/lib/api', () => ({
  api: { deleteTimeline: vi.fn() },
}))

function wrap(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>)
}

function makeTimeline(overrides: Partial<Timeline> = {}): Timeline {
  return {
    id: 'tl-fork',
    session_id: 's-1',
    parent_timeline_id: 'root',
    fork_at_step: 3,
    created_at: '2026-04-14T10:00:00Z',
    label: 'throwaway',
    ...overrides,
  }
}

beforeEach(() => {
  vi.clearAllMocks()
  useStore.setState({ selectedTimelineId: null })
})

afterEach(() => {
  useStore.setState({ selectedTimelineId: null })
})

describe('DeleteTimelineConfirmModal', () => {
  it('does not render when closed', () => {
    wrap(
      <DeleteTimelineConfirmModal
        isOpen={false}
        onClose={() => {}}
        sessionId="s-1"
        timeline={makeTimeline()}
      />,
    )
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('does not render when timeline is null', () => {
    wrap(
      <DeleteTimelineConfirmModal
        isOpen={true}
        onClose={() => {}}
        sessionId="s-1"
        timeline={null}
      />,
    )
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('shows the fork label and fork-at-step in the confirmation body', () => {
    wrap(
      <DeleteTimelineConfirmModal
        isOpen={true}
        onClose={() => {}}
        sessionId="s-1"
        timeline={makeTimeline({ label: 'my-fork', fork_at_step: 5 })}
      />,
    )
    expect(screen.getByText('my-fork')).toBeInTheDocument()
    expect(screen.getByText(/from step 5/)).toBeInTheDocument()
  })

  it('calls api.deleteTimeline with (sessionId, timelineId) on confirm and closes', async () => {
    const deleteMock = vi.mocked(api.deleteTimeline)
    deleteMock.mockResolvedValue({ deleted: true })
    const onClose = vi.fn()

    wrap(
      <DeleteTimelineConfirmModal
        isOpen={true}
        onClose={onClose}
        sessionId="s-1"
        timeline={makeTimeline({ id: 'fork-a' })}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /delete fork/i }))

    await waitFor(() => expect(deleteMock).toHaveBeenCalledWith('s-1', 'fork-a'))
    await waitFor(() => expect(onClose).toHaveBeenCalled())
  })

  it('navigates to the parent timeline when the deleted fork was selected', async () => {
    const deleteMock = vi.mocked(api.deleteTimeline)
    deleteMock.mockResolvedValue({ deleted: true })
    useStore.setState({ selectedTimelineId: 'fork-a' })

    wrap(
      <DeleteTimelineConfirmModal
        isOpen={true}
        onClose={() => {}}
        sessionId="s-1"
        timeline={makeTimeline({ id: 'fork-a', parent_timeline_id: 'root' })}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /delete fork/i }))

    await waitFor(() => expect(deleteMock).toHaveBeenCalled())
    expect(useStore.getState().selectedTimelineId).toBe('root')
  })

  it('leaves the store selection untouched if another timeline was selected', async () => {
    const deleteMock = vi.mocked(api.deleteTimeline)
    deleteMock.mockResolvedValue({ deleted: true })
    useStore.setState({ selectedTimelineId: 'other-fork' })

    wrap(
      <DeleteTimelineConfirmModal
        isOpen={true}
        onClose={() => {}}
        sessionId="s-1"
        timeline={makeTimeline({ id: 'fork-a', parent_timeline_id: 'root' })}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /delete fork/i }))

    await waitFor(() => expect(deleteMock).toHaveBeenCalled())
    expect(useStore.getState().selectedTimelineId).toBe('other-fork')
  })

  it('keeps the modal open and shows the error when the server returns 409', async () => {
    const deleteMock = vi.mocked(api.deleteTimeline)
    deleteMock.mockRejectedValue(new Error('API error 409: Cannot delete fork while it has 2 child fork(s)'))
    const onClose = vi.fn()

    wrap(
      <DeleteTimelineConfirmModal
        isOpen={true}
        onClose={onClose}
        sessionId="s-1"
        timeline={makeTimeline()}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /delete fork/i }))

    await waitFor(() => expect(screen.getByText(/child fork/i)).toBeInTheDocument())
    expect(onClose).not.toHaveBeenCalled()
  })
})
