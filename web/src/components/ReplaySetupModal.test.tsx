import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ReplaySetupModal } from './ReplaySetupModal'
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

const originalClipboard = navigator.clipboard

beforeEach(() => {
  vi.clearAllMocks()
  useStore.setState({ selectedTimelineId: null })
})

afterEach(() => {
  useStore.setState({ selectedTimelineId: null })
  // Restore the original clipboard descriptor so tests that mutate it don't
  // leak into other tests via the shared navigator global.
  if (originalClipboard) {
    Object.assign(navigator, { clipboard: originalClipboard })
  } else {
    delete (navigator as unknown as { clipboard?: unknown }).clipboard
  }
})

describe('ReplaySetupModal', () => {
  it('defaults label to replay-from-{N} and titles the modal accordingly', () => {
    wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={() => {}}
        sessionId="abcdef12-3456"
        atStep={3}
      />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    expect(input.value).toBe('replay-from-3')
    expect(screen.getByText(/Set up replay from step #3/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /set up replay/i })).toBeInTheDocument()
  })

  it('after fork succeeds, shows instructions with the rewind replay command and does NOT navigate', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-replay-1' })
    const onClose = vi.fn()

    wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={onClose}
        sessionId="abcdef1234567890"
        atStep={4}
      />,
    )

    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))

    await waitFor(() => expect(forkMock).toHaveBeenCalled())

    // Instructions panel renders the CLI command, uses an 8-char session-id prefix
    // (matches MCP tool format), includes the step number and the pre-created
    // fork id (--fork-id) so the CLI reuses it instead of double-forking (issue #140).
    const cmdElement = await screen.findByText(/rewind replay abcdef12 --from 4 --fork-id tl-replay-1/)
    expect(cmdElement).toBeInTheDocument()

    // Cached-steps explainer is present (matches MCP's `message` format).
    expect(screen.getByText(/Steps 1–4 replay from cache/i)).toBeInTheDocument()

    // Modal stays open until the user dismisses; do NOT auto-close or navigate.
    expect(onClose).not.toHaveBeenCalled()
    expect(useStore.getState().selectedTimelineId).toBeNull()
  })

  it('Copy button writes the command to the clipboard', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-replay-2' })

    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })

    wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={() => {}}
        sessionId="deadbeefcafebabe"
        atStep={2}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))

    const copyBtn = await screen.findByRole('button', { name: /copy/i })
    fireEvent.click(copyBtn)

    await waitFor(() => expect(writeText).toHaveBeenCalledWith(
      'rewind replay deadbeef --from 2 --fork-id tl-replay-2',
    ))
  })

  it('Done button closes the modal and navigates to the new fork', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-replay-3' })
    const onClose = vi.fn()

    wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={onClose}
        sessionId="sess123456"
        atStep={1}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))
    const done = await screen.findByRole('button', { name: /^done$/i })
    fireEvent.click(done)

    expect(onClose).toHaveBeenCalled()
    expect(useStore.getState().selectedTimelineId).toBe('tl-replay-3')
  })

  it('shows error and stays on the input state when fork fails', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockRejectedValue(new Error('API error 400: bad step'))

    wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={() => {}}
        sessionId="sess999"
        atStep={5}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))
    await waitFor(() => expect(screen.getByText(/bad step/)).toBeInTheDocument())
    // Still on the input state — the Set-up-replay primary button is there.
    expect(screen.getByRole('button', { name: /set up replay/i })).toBeInTheDocument()
    expect(screen.queryByText(/rewind replay/)).not.toBeInTheDocument()
  })
})

describe('ReplaySetupModal — polish & hardening', () => {
  it('moves focus to the Done button after switching to instructions', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-focus' })

    wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={() => {}}
        sessionId="focus-session-id"
        atStep={2}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))

    const done = await screen.findByRole('button', { name: /^done$/i })
    await waitFor(() => expect(done).toHaveFocus())
  })

  it('announces success banner with aria-live=polite', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-aria' })

    wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={() => {}}
        sessionId="sess-aria"
        atStep={1}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))
    const alert = await screen.findByRole('status')
    expect(alert).toHaveAttribute('aria-live', 'polite')
    expect(alert.textContent).toMatch(/Fork created/)
  })

  it('does not call setState after the modal closes with the Copied timer pending', async () => {
    vi.useFakeTimers()
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-timer' })

    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })

    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {})

    const { unmount } = wrap(
      <ReplaySetupModal
        isOpen={true}
        onClose={() => {}}
        sessionId="timer-test"
        atStep={1}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))
    await vi.waitFor(() => screen.findByRole('button', { name: /copy/i }))
    fireEvent.click(screen.getByRole('button', { name: /copy/i }))
    await Promise.resolve()
    await Promise.resolve()

    unmount()
    vi.advanceTimersByTime(3000)

    const unmountedSetStateWarnings = errSpy.mock.calls.filter((args) =>
      String(args[0]).includes('unmounted component') ||
      String(args[0]).includes("can't perform a React state update"),
    )
    expect(unmountedSetStateWarnings).toHaveLength(0)

    errSpy.mockRestore()
    vi.useRealTimers()
  })
})
