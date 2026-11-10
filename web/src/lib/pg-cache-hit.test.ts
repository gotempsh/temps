// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  formatPgCacheHitRatio,
  metricTileValue,
  pgCacheHitRatioDescription,
  pgCacheHitRatioLabel,
  pgCacheHitRatioScopedLabel,
} from './pg-cache-hit'

describe('Postgres cache hit ratio label', () => {
  test('says the value covers the last 15 minutes', () => {
    expect(pgCacheHitRatioLabel()).toBe('Cache hit ratio (last 15 min)')
  })

  test('names the databases the value covers', () => {
    expect(pgCacheHitRatioScopedLabel('all')).toBe(
      'Cache hit ratio (last 15 min, all databases)'
    )
    expect(pgCacheHitRatioScopedLabel({ database: 'app_production' })).toBe(
      'Cache hit ratio (last 15 min, app_production)'
    )
  })

  test('explains that the server-wide value is weighted by traffic', () => {
    expect(pgCacheHitRatioDescription('all')).toContain(
      'across all databases on this server, weighted by traffic'
    )
    expect(pgCacheHitRatioDescription({ database: 'app' })).toContain(
      'in database app'
    )
  })
})

describe('formatPgCacheHitRatio', () => {
  test('renders the 0–1 ratio as a percentage', () => {
    expect(formatPgCacheHitRatio(0.991)).toBe('99.1%')
    expect(formatPgCacheHitRatio(0)).toBe('0.0%')
    expect(formatPgCacheHitRatio(1)).toBe('100.0%')
  })

  test('shows no value rather than a made-up one', () => {
    expect(formatPgCacheHitRatio(undefined)).toBe('—')
    expect(formatPgCacheHitRatio(null)).toBe('—')
    expect(formatPgCacheHitRatio(Number.NaN)).toBe('—')
  })

  test('an out-of-range alert threshold is shown as configured', () => {
    // A rule entered as 95 (meaning 95%) must not pass for "100.0%".
    expect(formatPgCacheHitRatio(95)).toBe('9500.0%')
    expect(formatPgCacheHitRatio(-0.1)).toBe('-10.0%')
  })
})

describe('metricTileValue', () => {
  const lookup = (values: Record<string, number>) => (metric: string) =>
    values[metric]

  test('an empty window hides a ratio stored before it', () => {
    const values = lookup({
      'pg.cache_hit_ratio': 0.3,
      'pg.cache_blocks_accessed': 0,
    })
    expect(metricTileValue('pg.cache_hit_ratio', values)).toBeUndefined()
  })

  test('a window with traffic shows its ratio', () => {
    const values = lookup({
      'pg.cache_hit_ratio': 0.99,
      'pg.cache_blocks_accessed': 150_000,
    })
    expect(metricTileValue('pg.cache_hit_ratio', values)).toBe(0.99)
  })

  test('without a volume reading the ratio is shown as stored', () => {
    expect(
      metricTileValue('pg.cache_hit_ratio', lookup({ 'pg.cache_hit_ratio': 1 }))
    ).toBe(1)
  })

  test('other metrics are passed through', () => {
    const values = lookup({
      'pg.connections': 4,
      'pg.cache_blocks_accessed': 0,
    })
    expect(metricTileValue('pg.connections', values)).toBe(4)
  })
})
