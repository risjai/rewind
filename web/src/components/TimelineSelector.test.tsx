import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { TimelineSelector } from './TimelineSelector'
import { useStore } from '@/hooks/use-store'
import type { Timeline } from '@/types/api'

// The Delete button mounts DeleteTimelineConfirmModal, which pulls in the
// api module even though we never actually click Delete in these tests.
vi.mock('@/lib/api', () => ({
  api: { deleteTimeline: vi.fn() },
}))

function renderWithClient(ui: React.ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>)
}

function makeTimeline(overrides: Partial<Timeline> = {}): Timeline {
  return {
    id: 'tl-1',
    session_id: 's-1',
    parent_timeline_id: null,
    fork_at_step: null,
    created_at: '2026-04-14T10:00:00Z',
    label: 'main',
    ...overrides,
  }
}

beforeEach(() => {
  useStore.setState({ selectedTimelineId: null, view: 'sessions' })
  window.location.hash = ''
})

afterEach(() => {
  window.location.hash = ''
})

describe('TimelineSelector — Diff against parent button (Phase 3)', () => {
  const root = makeTimeline({ id: 'root', label: 'main' })
  const fork = makeTimeline({ id: 'fork', parent_timeline_id: 'root', fork_at_step: 3, label: 'fork-at-3' })

  it('does not show the Diff button when the active timeline is root', () => {
    // No selected timeline → defaults to root.
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    expect(screen.queryByRole('button', { name: /diff against parent/i })).toBeNull()
  })

  it('shows the Diff button when the active timeline has a parent', () => {
    useStore.setState({ selectedTimelineId: 'fork' })
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    expect(screen.getByRole('button', { name: /diff against parent/i })).toBeInTheDocument()
  })

  it('sets URL hash to #/diff/{session}/{parent}/{active} and switches view', () => {
    useStore.setState({ selectedTimelineId: 'fork' })
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    fireEvent.click(screen.getByRole('button', { name: /diff against parent/i }))
    expect(useStore.getState().view).toBe('diff')
    // Hash is the source of truth for left/right — shareable/bookmarkable.
    expect(window.location.hash).toBe('#/diff/s-1/root/fork')
  })

  it('uses history.replaceState so clicking Diff does not add a back-stack entry', () => {
    useStore.setState({ selectedTimelineId: 'fork' })
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    const before = window.history.length
    fireEvent.click(screen.getByRole('button', { name: /diff against parent/i }))
    // replaceState does not grow history; pushState would bump this by 1.
    expect(window.history.length).toBe(before)
  })

  it('URL-encodes timeline IDs in the hash (defensive for future non-UUID IDs)', () => {
    const weirdId = 'my/fork with space'
    const weirdFork = makeTimeline({
      id: weirdId,
      parent_timeline_id: 'root',
      fork_at_step: 1,
      label: 'weird',
    })
    useStore.setState({ selectedTimelineId: weirdId })
    renderWithClient(<TimelineSelector timelines={[root, weirdFork]} />)
    fireEvent.click(screen.getByRole('button', { name: /diff against parent/i }))
    // jsdom round-trips hash with encoded segments preserved.
    expect(window.location.hash).toBe(
      `#/diff/s-1/root/${encodeURIComponent(weirdId)}`,
    )
  })

  it('clicking a timeline pill updates the store selection', () => {
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    fireEvent.click(screen.getByText('fork-at-3'))
    expect(useStore.getState().selectedTimelineId).toBe('fork')
  })
})

describe('TimelineSelector — Delete fork trash icon (#143)', () => {
  const root = makeTimeline({ id: 'root', label: 'main' })
  const fork = makeTimeline({ id: 'fork', parent_timeline_id: 'root', fork_at_step: 3, label: 'fork-at-3' })

  it('renders a Delete button only for fork pills, not the root', () => {
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    // One delete button per fork (root is excluded).
    const deleteButtons = screen.getAllByRole('button', { name: /^delete fork/i })
    expect(deleteButtons).toHaveLength(1)
    // The button's aria-label contains the fork's label so screen readers
    // announce which fork they're about to delete.
    expect(deleteButtons[0]).toHaveAttribute('aria-label', 'Delete fork fork-at-3')
  })

  it('clicking the trash icon opens the confirmation dialog with the right fork', () => {
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    fireEvent.click(screen.getByRole('button', { name: /^delete fork fork-at-3/i }))
    // Dialog is shown; the fork's fork_at_step appears in the body ("from step 3").
    const dialog = screen.getByRole('dialog', { name: /delete fork/i })
    expect(dialog).toBeInTheDocument()
    expect(dialog).toHaveTextContent(/from step 3/)
  })

  it('clicking the trash icon does NOT also fire the pill selection', () => {
    // Important: the pill <button> is a sibling of the trash <button>, not a
    // parent, so clicks on the trash icon must not bubble to a pill click.
    renderWithClient(<TimelineSelector timelines={[root, fork]} />)
    useStore.setState({ selectedTimelineId: null })
    fireEvent.click(screen.getByRole('button', { name: /^delete fork fork-at-3/i }))
    // Selection is still null — only the dialog opened.
    expect(useStore.getState().selectedTimelineId).toBeNull()
  })
})
