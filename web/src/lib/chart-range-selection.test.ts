import { describe, expect, test } from 'bun:test'
import {
  formatChartDateRange,
  orderedChartDateRange,
} from './chart-range-selection'

describe('orderedChartDateRange', () => {
  test('orders a selection dragged from right to left', () => {
    const range = orderedChartDateRange(
      '2026-08-17T09:00:00.000Z',
      '2026-08-17T07:00:00.000Z'
    )

    expect(range?.from.toISOString()).toBe('2026-08-17T07:00:00.000Z')
    expect(range?.to.toISOString()).toBe('2026-08-17T09:00:00.000Z')
  })

  test('accepts epoch timestamps from analytics chart points', () => {
    const range = orderedChartDateRange(1_776_583_800_000, 1_776_587_400_000)

    expect(range).not.toBeNull()
    expect(range!.to.getTime() - range!.from.getTime()).toBe(3_600_000)
  })

  test('rejects a click without a range and invalid values', () => {
    expect(
      orderedChartDateRange(1_776_583_800_000, 1_776_583_800_000)
    ).toBeNull()
    expect(orderedChartDateRange('not-a-date', null)).toBeNull()
  })

  test('formats the selected timestamps for the chart label', () => {
    const label = formatChartDateRange({
      from: new Date(2026, 7, 16, 17, 0),
      to: new Date(2026, 7, 17, 5, 0),
    })

    expect(label).toContain('Aug 16, 2026 · 17:00')
    expect(label).toContain('→ Aug 17, 2026 · 05:00')
  })
})
