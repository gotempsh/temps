// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  formatPgCacheHitRatio,
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

  test('never shows a value outside 0–100%', () => {
    expect(formatPgCacheHitRatio(1.2)).toBe('100.0%')
    expect(formatPgCacheHitRatio(-0.1)).toBe('0.0%')
  })
})
