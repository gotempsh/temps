// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  pickProxyBucketInterval,
  PROXY_MAX_CHART_POINTS,
  proxyWindowTooWide,
  resolveProxyWindow,
} from './proxy-metrics-window'

const NOW = new Date('2026-08-13T12:00:00.000Z')

describe('pickProxyBucketInterval', () => {
  test('matches node-metric preset resolution', () => {
    expect(pickProxyBucketInterval(3_600)).toBe('1 minute')
    expect(pickProxyBucketInterval(21_600)).toBe('5 minutes')
    expect(pickProxyBucketInterval(86_400)).toBe('15 minutes')
    expect(pickProxyBucketInterval(604_800)).toBe('1 hour')
  })

  test('stays under PROXY_MAX_CHART_POINTS through 30d', () => {
    const stepSeconds: Record<string, number> = {
      '1 minute': 60,
      '5 minutes': 300,
      '15 minutes': 900,
      '1 hour': 3_600,
    }
    for (const secs of [3_600, 21_600, 86_400, 604_800, 30 * 86_400]) {
      const interval = pickProxyBucketInterval(secs)
      const step = stepSeconds[interval]
      expect(step).toBeDefined()
      expect(Math.ceil(secs / step)).toBeLessThanOrEqual(PROXY_MAX_CHART_POINTS)
    }
  })
})

describe('resolveProxyWindow', () => {
  test('preset 1h anchors to now and keeps the range param', () => {
    const w = resolveProxyWindow('1h', undefined, NOW)
    expect(w.rangeParam).toBe('1h')
    expect(w.durationSeconds).toBe(3_600)
    expect(w.bucketInterval).toBe('1 minute')
    expect(w.endIso).toBe(NOW.toISOString())
    expect(w.startIso).toBe(new Date(NOW.getTime() - 3_600_000).toISOString())
    expect(w.showDate).toBe(false)
  })

  test('7d shows calendar dates on the axis', () => {
    const w = resolveProxyWindow('7d', undefined, NOW)
    expect(w.rangeParam).toBe('7d')
    expect(w.showDate).toBe(true)
    expect(w.bucketInterval).toBe('1 hour')
  })

  test('custom range omits rangeParam and sizes the bucket from span', () => {
    const from = new Date('2026-08-13T08:00:00.000Z')
    const to = new Date('2026-08-13T12:00:00.000Z')
    const w = resolveProxyWindow('custom', { from, to }, NOW)
    expect(w.rangeParam).toBeUndefined()
    expect(w.durationSeconds).toBe(14_400)
    expect(w.bucketInterval).toBe('5 minutes')
    expect(w.startIso).toBe(from.toISOString())
    expect(w.endIso).toBe(to.toISOString())
    expect(w.showDate).toBe(false)
  })

  test('multi-day custom range shows dates', () => {
    const from = new Date('2026-08-10T12:00:00.000Z')
    const to = new Date('2026-08-13T12:00:00.000Z')
    const w = resolveProxyWindow('custom', { from, to }, NOW)
    expect(w.showDate).toBe(true)
    expect(w.bucketInterval).toBe('1 hour')
  })

  test('incomplete custom range falls back to 24h', () => {
    const w = resolveProxyWindow('custom', { from: NOW }, NOW)
    expect(w.rangeParam).toBe('24h')
    expect(w.durationSeconds).toBe(86_400)
  })
})

describe('proxyWindowTooWide', () => {
  test('unscoped cap is 7 days', () => {
    const from = new Date('2026-08-01T12:00:00.000Z')
    const to = new Date('2026-08-13T12:00:00.000Z')
    const w = resolveProxyWindow('custom', { from, to }, NOW)
    expect(proxyWindowTooWide(w, false)).toBe(true)
    expect(proxyWindowTooWide(w, true)).toBe(false)
  })

  test('7d preset is inside the unscoped cap', () => {
    const w = resolveProxyWindow('7d', undefined, NOW)
    expect(proxyWindowTooWide(w, false)).toBe(false)
  })
})
