// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  formatShare,
  changePercent,
  comparableDeployments,
  deploymentMarkers,
  hourTick,
  trendChartRows,
} from './operation-trend'

const point = (bucket_start: string, p95_ms: number) => ({
  bucket_start,
  count: 10,
  error_count: 0,
  p50_ms: p95_ms / 2,
  p95_ms,
})

describe('trendChartRows', () => {
  test('orders hours oldest first and keeps each x value unique', () => {
    const rows = trendChartRows([
      point('2026-11-20T11:00:00Z', 200),
      point('2026-11-20T10:00:00Z', 100),
    ])
    expect(rows.map((r) => r.p95)).toEqual([100, 200])
    expect(new Set(rows.map((r) => r.hour)).size).toBe(2)
    expect(hourTick(rows[0].hour)).not.toContain('2026-11-20T10')
  })
})

describe('deploymentMarkers', () => {
  const rows = trendChartRows([
    point('2026-11-20T10:00:00Z', 100),
    point('2026-11-20T12:00:00Z', 100),
  ])

  test('snaps a deployment to the nearest hour', () => {
    const markers = deploymentMarkers(rows, [
      { deployment_id: 7, slug: 'deploy-7', live_at: '2026-11-20T11:50:00Z' },
    ])
    expect(markers).toHaveLength(1)
    expect(markers[0].x).toBe(rows[1].hour)
    expect(markers[0].label).toBe('#7')
  })

  test('collapses deployments that land on the same hour', () => {
    const markers = deploymentMarkers(rows, [
      { deployment_id: 7, slug: 'a', live_at: '2026-11-20T10:05:00Z' },
      { deployment_id: 8, slug: 'b', live_at: '2026-11-20T10:10:00Z' },
    ])
    expect(markers).toHaveLength(1)
    expect(markers[0].label).toBe('#7 +1')
    expect(markers[0].title).toContain('#8')
  })

  test('draws nothing without rows', () => {
    expect(
      deploymentMarkers(
        [],
        [{ deployment_id: 1, slug: 'x', live_at: '2026-11-20T10:00:00Z' }]
      )
    ).toEqual([])
  })
})

describe('changePercent', () => {
  test('signs the change and rounds it', () => {
    expect(changePercent(0.404)).toBe('+40%')
    expect(changePercent(-0.3)).toBe('−30%')
    expect(changePercent(0)).toBe('0%')
    expect(changePercent(null)).toBe('—')
  })
})

describe('comparableDeployments', () => {
  test('keeps deployments that went live, newest first', () => {
    const list = comparableDeployments([
      { id: 1, status: 'deployed', created_at: '2026-11-20T10:00:01Z' },
      { id: 2, status: 'failed', created_at: '2026-11-20T10:00:03Z' },
      // Fractional seconds sort after the whole second as text, not in time.
      { id: 3, status: 'completed', created_at: '2026-11-20T10:00:01.5Z' },
      { id: 4, status: 'pending', created_at: '2026-11-20T10:00:04Z' },
    ])
    expect(list.map((d) => d.id)).toEqual([3, 1])
  })
})

describe('formatShare', () => {
  test('rounds to whole percents and keeps tiny shares visible', () => {
    expect(formatShare(0.4249)).toBe('42%')
    expect(formatShare(0.004)).toBe('<1%')
    expect(formatShare(0)).toBe('0%')
    expect(formatShare(1.3)).toBe('130%')
    expect(formatShare(undefined)).toBe('—')
  })
})
