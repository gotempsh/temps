import { describe, expect, test } from 'bun:test'
import { orderedChartDateRange } from './chart-range-selection'

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
})
