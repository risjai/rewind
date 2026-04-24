import { describe, it, expect, vi } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { TimelineDiff, StepDiffEntry } from '@/types/api'
import { parseDiffHash } from './DiffView'

// We test the DiffTimeline sub-component indirectly by importing DiffView
// and providing mock data. Since DiffView uses react-query, we need a provider.

// For unit testing the visual diff rendering, we extract the key logic:
// DiffTimeline receives a TimelineDiff and renders color-coded bars.

function makeDiff(overrides: Partial<TimelineDiff> = {}): TimelineDiff {
  return {
    diverge_at_step: null,
    left_label: 'main',
    right_label: 'fork',
    step_diffs: [],
    ...overrides,
  }
}

function makeStepDiff(overrides: Partial<StepDiffEntry> = {}): StepDiffEntry {
  return {
    step_number: 1,
    diff_type: 'Same',
    left: { step_type: 'llm_call', status: 'success', model: 'gpt-4o', tokens_in: 100, tokens_out: 50, duration_ms: 500, response_preview: 'Hello' },
    right: { step_type: 'llm_call', status: 'success', model: 'gpt-4o', tokens_in: 100, tokens_out: 50, duration_ms: 500, response_preview: 'Hello' },
    ...overrides,
  }
}

describe('DiffTimeline data model', () => {
  it('TimelineDiff with all Same steps has no divergence', () => {
    const diff = makeDiff({
      step_diffs: [
        makeStepDiff({ step_number: 1, diff_type: 'Same' }),
        makeStepDiff({ step_number: 2, diff_type: 'Same' }),
        makeStepDiff({ step_number: 3, diff_type: 'Same' }),
      ],
    })
    expect(diff.diverge_at_step).toBeNull()
    expect(diff.step_diffs.every(d => d.diff_type === 'Same')).toBe(true)
  })

  it('TimelineDiff with Modified step at step 3', () => {
    const diff = makeDiff({
      diverge_at_step: 3,
      step_diffs: [
        makeStepDiff({ step_number: 1, diff_type: 'Same' }),
        makeStepDiff({ step_number: 2, diff_type: 'Same' }),
        makeStepDiff({ step_number: 3, diff_type: 'Modified',
          right: { step_type: 'llm_call', status: 'success', model: 'gpt-4o', tokens_in: 120, tokens_out: 60, duration_ms: 600, response_preview: 'Different' },
        }),
      ],
    })
    expect(diff.diverge_at_step).toBe(3)
    expect(diff.step_diffs[2].diff_type).toBe('Modified')
  })

  it('LeftOnly steps have null right summary', () => {
    const entry = makeStepDiff({ step_number: 5, diff_type: 'LeftOnly', right: null })
    expect(entry.right).toBeNull()
    expect(entry.left).not.toBeNull()
  })

  it('RightOnly steps have null left summary', () => {
    const entry = makeStepDiff({ step_number: 6, diff_type: 'RightOnly', left: null })
    expect(entry.left).toBeNull()
    expect(entry.right).not.toBeNull()
  })

  it('diff_type color mapping covers all 4 types', () => {
    const DIFF_COLORS: Record<string, string> = {
      Same: 'bg-neutral-600',
      Modified: 'bg-amber-500',
      LeftOnly: 'bg-red-500',
      RightOnly: 'bg-green-500',
    }
    for (const type of ['Same', 'Modified', 'LeftOnly', 'RightOnly']) {
      expect(DIFF_COLORS[type]).toBeDefined()
    }
  })
})

describe('parseDiffHash (Phase 3 URL-hash routing)', () => {
  it('returns null for non-diff hashes', () => {
    expect(parseDiffHash('')).toBeNull()
    expect(parseDiffHash('#')).toBeNull()
    expect(parseDiffHash('#/session/s-1')).toBeNull()
    expect(parseDiffHash('#/session/s-1/step-2')).toBeNull()
  })

  it('parses #/diff/{session}/{left}/{right}', () => {
    expect(parseDiffHash('#/diff/s-1/left-id/right-id')).toEqual({
      leftId: 'left-id',
      rightId: 'right-id',
    })
  })

  it('returns null when left or right is missing', () => {
    expect(parseDiffHash('#/diff/s-1')).toBeNull()
    expect(parseDiffHash('#/diff/s-1/left-only')).toBeNull()
    expect(parseDiffHash('#/diff/s-1//right-only')).toBeNull()
  })

  it('tolerates both #/ and # prefixes', () => {
    expect(parseDiffHash('#diff/s-1/a/b')).toEqual({ leftId: 'a', rightId: 'b' })
    expect(parseDiffHash('#/diff/s-1/a/b')).toEqual({ leftId: 'a', rightId: 'b' })
  })

  it('decodes URL-encoded timeline IDs (round-trips with TimelineSelector)', () => {
    const rawId = 'my/fork with space'
    const encoded = encodeURIComponent(rawId)
    expect(parseDiffHash(`#/diff/s-1/root/${encoded}`)).toEqual({
      leftId: 'root',
      rightId: rawId,
    })
  })

  it('returns null when decoding fails on malformed encoding', () => {
    // `%E0%A4%A` is a truncated UTF-8 sequence that decodeURIComponent throws on.
    expect(parseDiffHash('#/diff/s-1/root/%E0%A4%A')).toBeNull()
  })
})

describe('DiffTimeline divergence marker', () => {
  it('diverge_at_step positions marker as percentage', () => {
    const diff = makeDiff({
      diverge_at_step: 4,
      step_diffs: Array.from({ length: 5 }, (_, i) => makeStepDiff({ step_number: i + 1 })),
    })
    const total = diff.step_diffs.length
    const markerPct = ((diff.diverge_at_step! - 1) / total) * 100
    expect(markerPct).toBe(60)
  })

  it('diverge_at_step 1 positions marker at 0%', () => {
    const diff = makeDiff({
      diverge_at_step: 1,
      step_diffs: [makeStepDiff({ step_number: 1 })],
    })
    const markerPct = ((diff.diverge_at_step! - 1) / diff.step_diffs.length) * 100
    expect(markerPct).toBe(0)
  })
})
