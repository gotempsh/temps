// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MARIADB_FLAG_METRICS,
  formatFlagValue,
  MARIADB_METRIC_GROUPS,
  MARIADB_METRIC_LABELS,
  MARIADB_METRICS,
  MARIADB_PER_DATABASE_GROUPS,
} from './mariadb-metrics'

describe('MariaDB metric groups', () => {
  test('every metric is listed once', () => {
    expect(new Set(MARIADB_METRICS).size).toBe(MARIADB_METRICS.length)
  })

  test('per-schema metrics are kept out of the instance groups', () => {
    const instance = new Set(MARIADB_METRIC_GROUPS.flatMap((g) => g.metrics))
    for (const metric of MARIADB_PER_DATABASE_GROUPS.flatMap(
      (g) => g.metrics
    )) {
      expect(instance.has(metric)).toBe(false)
    }
  })

  test('labels and flags only name collected metrics', () => {
    const known = new Set(MARIADB_METRICS)
    for (const metric of [
      ...Object.keys(MARIADB_METRIC_LABELS),
      ...MARIADB_FLAG_METRICS,
    ]) {
      expect(known.has(metric)).toBe(true)
    }
  })
})

describe('formatFlagValue', () => {
  test('names exact 1 and 0 as Yes and No', () => {
    expect(formatFlagValue(1)).toBe('Yes')
    expect(formatFlagValue(0)).toBe('No')
  })

  test('leaves averaged values to numeric formatting', () => {
    expect(formatFlagValue(0.5)).toBeNull()
    expect(formatFlagValue(0.99)).toBeNull()
  })
})
