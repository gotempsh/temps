import { test, expect, describe } from 'bun:test'
import {
  formatMsForTest,
  formatPercentForTest,
  parseNonNegativeIntegerForTest,
  parsePageSizeForTest,
  parseTrafficFilterForTest,
  parseTrafficMetricsForTest,
  trafficOffsetPlanForTest,
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
    expect(() =>
      parseNonNegativeIntegerForTest('abc', 'Environment ID'),
    ).toThrow()
  })

  test('requires API page sizes between 1 and 100', () => {
    expect(parsePageSizeForTest(undefined)).toBe(20)
    expect(parsePageSizeForTest('100')).toBe(100)
    expect(() => parsePageSizeForTest('0')).toThrow('between 1 and 100')
    expect(() => parsePageSizeForTest('101')).toThrow('between 1 and 100')
  })
})

describe('offset pagination', () => {
  test('preserves non-page-aligned offsets', () => {
    expect(trafficOffsetPlanForTest(0)).toEqual({ page: 1, skip: 0 })
    expect(trafficOffsetPlanForTest(5)).toEqual({ page: 1, skip: 5 })
    expect(trafficOffsetPlanForTest(105)).toEqual({ page: 2, skip: 5 })
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

describe('traffic filters', () => {
  test('preserves colons inside filter values', () => {
    expect(parseTrafficFilterForTest('path:eq:/api/items:latest')).toEqual({
      dimension: 'path',
      operator: 'eq',
      values: ['/api/items:latest'],
    })
  })

  test('parses an in filter and rejects unknown dimensions', () => {
    expect(parseTrafficFilterForTest('status_class:in:4xx,5xx').values).toEqual(
      ['4xx', '5xx'],
    )
    expect(() => parseTrafficFilterForTest('raw_sql:eq:anything')).toThrow(
      'Unknown filter dimension',
    )
  })
})

describe('crawler traffic metrics', () => {
  test('accepts bot and robots.txt request counts in generic queries', () => {
    expect(
      parseTrafficMetricsForTest('requests,bot_requests,robots_txt_requests'),
    ).toEqual(['requests', 'bot_requests', 'robots_txt_requests'])
  })
})
