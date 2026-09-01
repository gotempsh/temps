// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  buildPerformanceQuery,
  fetchPerformanceData,
  formatGroupKeyForTest,
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
    for (const tooLarge of ['2147483648', '9007199254740992']) {
      expect(() => parsePositiveIntegerForTest(tooLarge, 'Environment ID')).toThrow(
        /between 1 and 2147483647/
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
    for (const nonRfc3339 of ['08/01/2026', 'August 1, 2026', '2026-08-01 00:00:00']) {
      expect(() =>
        resolvePerformanceWindow({
          startDate: nonRfc3339,
          endDate: '2026-08-08T00:00:00Z',
        })
      ).toThrow(/valid RFC 3339/)
    }
    for (const invalidRfc3339 of [
      '2026-02-29T00:00:00Z',
      '2026-02-30T00:00:00Z',
      '2026-08-01T24:00:00Z',
      '2026-08-01T00:60:00Z',
      '2026-08-01T00:00:60Z',
      '2026-08-01T00:00:00+24:00',
    ]) {
      expect(() =>
        resolvePerformanceWindow({
          startDate: invalidRfc3339,
          endDate: '2026-08-08T00:00:00Z',
        })
      ).toThrow(/valid RFC 3339/)
    }
    expect(() => resolvePerformanceWindow({ startDate: '', endDate: '' })).toThrow(
      /valid RFC 3339/
    )
    expect(() =>
      resolvePerformanceWindow({ startDate: '', endDate: '2026-08-08T00:00:00Z' })
    ).toThrow(/valid RFC 3339/)
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

  test('sanitizes terminal controls in attacker-influenced breakdown keys', () => {
    expect(formatGroupKeyForTest('/safe\x1b[2J\x1b]52;c;Zm9yZ2Vk\x07\nnext\u202E')).toBe(
      '/safe next'
    )
  })
})

describe('Performance Insights endpoint orchestration', () => {
  const query = {
    project_id: 7,
    start_date: '2026-08-01T00:00:00.000Z',
    end_date: '2026-08-08T00:00:00.000Z',
    device_type: 'mobile',
    include_bots: false,
  }
  const timeline = {
    timestamps: [],
    lcp: [],
    inp: [],
    cls: [],
    fcp: [],
    ttfb: [],
    fid: [],
  }

  test('fetches summary and timeline without requesting a breakdown by default', async () => {
    const calls: string[] = []
    const result = await fetchPerformanceData(query, undefined, {
      getSummary: async (received) => {
        calls.push(`summary:${received.device_type}`)
        return { data: { lcp_p75: 1200 } }
      },
      getTimeline: async (received) => {
        calls.push(`timeline:${received.device_type}`)
        return { data: timeline }
      },
      getBreakdown: async () => {
        calls.push('breakdown')
        return { data: { grouped_by: 'path', groups: [], total_events: 0 } }
      },
    })

    expect(calls).toEqual(['summary:mobile', 'timeline:mobile'])
    expect(result.summary?.lcp_p75).toBe(1200)
    expect(result.breakdown).toBeUndefined()
  })

  test('forwards the shared filters and selected grouping to the breakdown request', async () => {
    let groupedQuery: Record<string, unknown> | undefined
    const result = await fetchPerformanceData(query, 'path', {
      getSummary: async () => ({ data: {} }),
      getTimeline: async () => ({ data: timeline }),
      getBreakdown: async (received) => {
        groupedQuery = received
        return {
          data: { grouped_by: 'path', groups: [], total_events: 0 },
        }
      },
    })

    expect(groupedQuery).toEqual({ ...query, group_by: 'path' })
    expect(result.breakdown?.grouped_by).toBe('path')
  })

  test('surfaces contextual errors from each performance endpoint', async () => {
    for (const failing of ['summary', 'timeline', 'breakdown'] as const) {
      const api = {
        getSummary: async () =>
          failing === 'summary' ? { error: { detail: 'summary unavailable' } } : { data: {} },
        getTimeline: async () =>
          failing === 'timeline'
            ? { error: { detail: 'timeline unavailable' } }
            : { data: timeline },
        getBreakdown: async () =>
          failing === 'breakdown'
            ? { error: { detail: 'breakdown unavailable' } }
            : { data: { grouped_by: 'path', groups: [], total_events: 0 } },
      }

      await expect(fetchPerformanceData(query, 'path', api)).rejects.toThrow(
        `${failing} unavailable`
      )
    }
  })
})
