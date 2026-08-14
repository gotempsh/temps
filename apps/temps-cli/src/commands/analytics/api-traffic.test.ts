import { test, expect, describe } from 'bun:test'
import {
  formatMsForTest,
  formatPercentForTest,
  parseNonNegativeIntegerForTest,
} from './api-traffic.js'

describe('formatMs', () => {
  test('renders "n/a" for null or undefined', () => {
    expect(formatMsForTest(null)).toBe('n/a')
    expect(formatMsForTest(undefined)).toBe('n/a')
  })

  test('renders a whole-millisecond value', () => {
    expect(formatMsForTest(42.7)).toBe('43ms')
  })

  test('renders zero, not "n/a" — zero latency is a real value, not missing data', () => {
    expect(formatMsForTest(0)).toBe('0ms')
  })
})

describe('numeric query options', () => {
  test('accepts zero and positive integers', () => {
    expect(parseNonNegativeIntegerForTest('0', 'Offset')).toBe(0)
    expect(parseNonNegativeIntegerForTest('42', 'Environment ID')).toBe(42)
  })

  test('rejects negative, fractional, and non-numeric values', () => {
    expect(() => parseNonNegativeIntegerForTest('-1', 'Offset')).toThrow()
    expect(() => parseNonNegativeIntegerForTest('1.5', 'Offset')).toThrow()
    expect(() => parseNonNegativeIntegerForTest('abc', 'Environment ID')).toThrow()
  })
})

describe('formatPercent', () => {
  test('renders a fraction as a one-decimal percentage', () => {
    expect(formatPercentForTest(0.055)).toBe('5.5%')
  })

  test('renders zero error rate explicitly', () => {
    expect(formatPercentForTest(0)).toBe('0.0%')
  })
})
