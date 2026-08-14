import { describe, expect, test } from 'bun:test'
import {
  buildPerformanceQuery,
  formatMetricForTest,
  parseDeviceForTest,
  parseGroupByForTest,
  parsePositiveIntegerForTest,
  resolvePerformanceWindow,
} from './performance.js'

describe('Performance Insights option validation', () => {
  test('accepts supported devices and rejects unknown device values', () => {
    expect(parseDeviceForTest('desktop')).toBe('desktop')
    expect(parseDeviceForTest('mobile')).toBe('mobile')
    expect(parseDeviceForTest(undefined)).toBeUndefined()
    expect(() => parseDeviceForTest('tablet')).toThrow(/desktop, mobile/)
  })

  test('accepts every server-supported breakdown dimension', () => {
    for (const dimension of [
      'path',
      'country',
      'region',
      'city',
      'device_type',
      'browser',
      'operating_system',
    ] as const) {
      expect(parseGroupByForTest(dimension)).toBe(dimension)
    }
    expect(() => parseGroupByForTest('device')).toThrow(/device_type/)
  })

  test('requires positive environment and deployment IDs', () => {
    expect(parsePositiveIntegerForTest('42', 'Environment ID')).toBe(42)
    for (const invalid of ['0', '-1', '1.5', 'abc']) {
      expect(() => parsePositiveIntegerForTest(invalid, 'Environment ID')).toThrow(
        /positive integer/
      )
    }
  })
})

describe('Performance Insights date windows', () => {
  test('uses a period when no explicit dates are provided', () => {
    const window = resolvePerformanceWindow({ period: '24h' })
    expect(window.period).toBe('24h')
    expect(Date.parse(window.endDate) - Date.parse(window.startDate)).toBeCloseTo(
      24 * 60 * 60 * 1000,
      -2
    )
  })

  test('normalizes an explicit RFC 3339 range', () => {
    const window = resolvePerformanceWindow({
      startDate: '2026-08-01T00:00:00Z',
      endDate: '2026-08-08T00:00:00Z',
    })
    expect(window.startDate).toBe('2026-08-01T00:00:00.000Z')
    expect(window.endDate).toBe('2026-08-08T00:00:00.000Z')
    expect(window.period).toBeUndefined()
  })

  test('rejects incomplete, invalid, and reversed explicit ranges', () => {
    expect(() => resolvePerformanceWindow({ startDate: '2026-08-01T00:00:00Z' })).toThrow(
      /provided together/
    )
    expect(() =>
      resolvePerformanceWindow({ startDate: 'not-a-date', endDate: '2026-08-08T00:00:00Z' })
    ).toThrow(/valid RFC 3339/)
    expect(() =>
      resolvePerformanceWindow({
        startDate: '2026-08-08T00:00:00Z',
        endDate: '2026-08-01T00:00:00Z',
      })
    ).toThrow(/earlier than end date/)
  })
})

describe('Performance Insights API query', () => {
  test('maps every CLI filter to the performance endpoint query', () => {
    const window = {
      startDate: '2026-08-01T00:00:00.000Z',
      endDate: '2026-08-08T00:00:00.000Z',
      label: 'test window',
    }
    expect(
      buildPerformanceQuery(7, window, {
        environmentId: '11',
        deploymentId: '13',
        device: 'mobile',
        includeBots: true,
        path: '/pricing',
        country: 'Spain',
        region: 'Madrid',
        city: 'Madrid',
        browser: 'Chrome',
        os: 'Android',
      })
    ).toEqual({
      project_id: 7,
      start_date: window.startDate,
      end_date: window.endDate,
      environment_id: 11,
      deployment_id: 13,
      device_type: 'mobile',
      include_bots: true,
      filter_path: '/pricing',
      filter_country: 'Spain',
      filter_region: 'Madrid',
      filter_city: 'Madrid',
      filter_browser: 'Chrome',
      filter_operating_system: 'Android',
    })
  })

  test('defaults bot samples off and leaves optional filters unset', () => {
    const window = resolvePerformanceWindow({ period: '7d' })
    const query = buildPerformanceQuery(7, window, {})
    expect(query.include_bots).toBe(false)
    expect(query.device_type).toBeUndefined()
    expect(query.environment_id).toBeUndefined()
  })
})

describe('Performance Insights formatting', () => {
  test('formats timing, CLS, missing, and zero values without conflating them', () => {
    expect(formatMetricForTest(2499.6, 'ms')).toBe('2500ms')
    expect(formatMetricForTest(0.08321, 'score')).toBe('0.083')
    expect(formatMetricForTest(null, 'ms')).toBe('n/a')
    expect(formatMetricForTest(0, 'ms')).toBe('0ms')
  })
})
