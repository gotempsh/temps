import { describe, expect, test } from 'bun:test'
import {
  computeTracesTimeWindow,
  tracesListTimeBounds,
  type TracesTimeRange,
} from './traces-time-window'

const FIXED_NOW = new Date('2026-08-04T12:00:00.000Z')

describe('computeTracesTimeWindow', () => {
  const cases: Array<[TracesTimeRange, string]> = [
    ['1h', '2026-08-04T11:00:00.000Z'],
    ['6h', '2026-08-04T06:00:00.000Z'],
    ['24h', '2026-08-03T12:00:00.000Z'],
    ['7d', '2026-07-28T12:00:00.000Z'],
    ['30d', '2026-07-05T12:00:00.000Z'],
  ]

  for (const [range, expectedStart] of cases) {
    test(`${range} anchors start relative to now`, () => {
      const window = computeTracesTimeWindow(range, FIXED_NOW)
      expect(window.endTime).toBe(FIXED_NOW.toISOString())
      expect(window.startTime).toBe(expectedStart)
    })
  }

  test('a later now advances end_time (refresh contract)', () => {
    const first = computeTracesTimeWindow('24h', FIXED_NOW)
    const later = new Date('2026-08-04T12:05:00.000Z')
    const second = computeTracesTimeWindow('24h', later)

    expect(second.endTime).toBe(later.toISOString())
    expect(second.endTime > first.endTime).toBe(true)
    // Span ingested at 12:02 would miss first.end (12:00) but land in second.
    const ingestedAt = '2026-08-04T12:02:00.000Z'
    expect(ingestedAt > first.endTime).toBe(true)
    expect(ingestedAt <= second.endTime).toBe(true)
  })
})

describe('tracesListTimeBounds', () => {
  const window = computeTracesTimeWindow('24h', FIXED_NOW)

  test('applies the window when not searching by trace id', () => {
    expect(tracesListTimeBounds(undefined, window)).toEqual({
      start_time: window.startTime,
      end_time: window.endTime,
    })
    expect(tracesListTimeBounds('', window)).toEqual({
      start_time: window.startTime,
      end_time: window.endTime,
    })
  })

  test('omits the window when searching by trace id', () => {
    expect(tracesListTimeBounds('abc123def456', window)).toEqual({
      start_time: undefined,
      end_time: undefined,
    })
  })
})
