import { test, expect, describe } from 'bun:test'
import { calculateDuration } from './status.js'

describe('calculateDuration', () => {
  test('renders sub-minute durations as seconds only', () => {
    expect(calculateDuration(0, 30)).toBe('30s')
  })

  test('renders minutes and remaining seconds once over a minute', () => {
    expect(calculateDuration(0, 90)).toBe('1m 30s')
  })

  test('renders an exact minute boundary with 0 remaining seconds', () => {
    expect(calculateDuration(0, 60)).toBe('1m 0s')
  })

  test('has no hour tier — durations over an hour still render as minutes', () => {
    // 3661s = 1h 1m 1s, but the formatter caps out at minutes.
    expect(calculateDuration(0, 3661)).toBe('61m 1s')
  })
})
