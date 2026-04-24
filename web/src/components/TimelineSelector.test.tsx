import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import { TimelineSelector } from './TimelineSelector'
import { useStore } from '@/hooks/use-store'
import type { Timeline } from '@/types/api'

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
    render(<TimelineSelector timelines={[root, fork]} />)
    expect(screen.queryByRole('button', { name: /diff against parent/i })).toBeNull()
  })

  it('shows the Diff button when the active timeline has a parent', () => {
    useStore.setState({ selectedTimelineId: 'fork' })
    render(<TimelineSelector timelines={[root, fork]} />)
    expect(screen.getByRole('button', { name: /diff against parent/i })).toBeInTheDocument()
  })

  it('sets URL hash to #/diff/{session}/{parent}/{active} and switches view', () => {
    useStore.setState({ selectedTimelineId: 'fork' })
    render(<TimelineSelector timelines={[root, fork]} />)
    fireEvent.click(screen.getByRole('button', { name: /diff against parent/i }))
    expect(useStore.getState().view).toBe('diff')
    // Hash is the source of truth for left/right — shareable/bookmarkable.
    expect(window.location.hash).toBe('#/diff/s-1/root/fork')
  })

  it('clicking a timeline pill updates the store selection', () => {
    render(<TimelineSelector timelines={[root, fork]} />)
    fireEvent.click(screen.getByText('fork-at-3'))
    expect(useStore.getState().selectedTimelineId).toBe('fork')
  })
})
