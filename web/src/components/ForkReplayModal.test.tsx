import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ForkReplayModal } from './ForkReplayModal'
import { api } from '@/lib/api'
import { useStore } from '@/hooks/use-store'

vi.mock('@/lib/api', () => ({
  api: { forkSession: vi.fn() },
}))

function wrap(ui: React.ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>)
}

beforeEach(() => {
  vi.clearAllMocks()
  useStore.setState({ selectedTimelineId: null })
})

afterEach(() => {
  useStore.setState({ selectedTimelineId: null })
})

describe('ForkReplayModal', () => {
  it('does not render when closed', () => {
    wrap(
      <ForkReplayModal
        isOpen={false}
        onClose={() => {}}
        mode="fork"
        sessionId="s1"
        atStep={3}
      />,
    )
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('does not render when atStep is null, even if isOpen is true', () => {
    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="fork"
        sessionId="s1"
        atStep={null}
      />,
    )
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('defaults label to fork-at-{N} and shows the step number', () => {
    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="fork"
        sessionId="s1"
        atStep={7}
      />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    expect(input.value).toBe('fork-at-7')
    expect(screen.getByText(/Fork from step #7/)).toBeInTheDocument()
  })

  it('submits fork, selects the new timeline, and closes on success', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'new-tl-1' })
    const onClose = vi.fn()

    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={onClose}
        mode="fork"
        sessionId="s1"
        timelineId="root-tl"
        atStep={4}
      />,
    )

    fireEvent.click(screen.getByRole('button', { name: /create fork/i }))

    await waitFor(() => expect(forkMock).toHaveBeenCalledWith('s1', {
      at_step: 4,
      label: 'fork-at-4',
      timeline_id: 'root-tl',
    }))
    await waitFor(() => expect(onClose).toHaveBeenCalled())
    expect(useStore.getState().selectedTimelineId).toBe('new-tl-1')
  })

  it('shows error when fork fails and keeps the modal open', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockRejectedValue(new Error('API error 400: bad step'))
    const onClose = vi.fn()

    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={onClose}
        mode="fork"
        sessionId="s1"
        atStep={4}
      />,
    )

    fireEvent.click(screen.getByRole('button', { name: /create fork/i }))

    await waitFor(() => expect(screen.getByText(/bad step/)).toBeInTheDocument())
    expect(onClose).not.toHaveBeenCalled()
    expect(useStore.getState().selectedTimelineId).toBeNull()
  })

  it('uses trimmed default label when input is cleared', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'new-tl-2' })

    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="fork"
        sessionId="s1"
        atStep={2}
      />,
    )

    const input = screen.getByRole('textbox') as HTMLInputElement
    fireEvent.change(input, { target: { value: '   ' } })
    fireEvent.click(screen.getByRole('button', { name: /create fork/i }))

    await waitFor(() => expect(forkMock).toHaveBeenCalledWith('s1', {
      at_step: 2,
      label: 'fork-at-2',
      timeline_id: undefined,
    }))
  })
})
