// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { buildLimitsFormSchema } from './build-limits'

const valid = {
  max_concurrent: 2,
  cpu_limit_cores: 0,
  memory_limit_mb: 0,
  build_cache_max_gb: 10,
  build_cache_max_unused_days: 7,
}

function issueFor(
  field: keyof typeof valid,
  value: number
): string | undefined {
  const result = buildLimitsFormSchema.safeParse({
    build_limits: { ...valid, [field]: value },
  })
  if (result.success) return undefined
  return result.error.issues.find(
    (issue) => issue.path.join('.') === `build_limits.${field}`
  )?.message
}

describe('buildLimitsFormSchema', () => {
  it('accepts the defaults, and 0 to turn the cache size cap off', () => {
    expect(
      buildLimitsFormSchema.safeParse({ build_limits: valid }).success
    ).toBe(true)
    expect(issueFor('build_cache_max_gb', 0)).toBeUndefined()
  })

  // A cleared number input reads as NaN and would be sent as null, which
  // the server refuses for the whole settings save.
  it('refuses a cleared cache size with the way to turn the cap off', () => {
    expect(issueFor('build_cache_max_gb', Number.NaN)).toBe(
      'Enter a value; use 0 to disable the size cap.'
    )
  })

  it('refuses every other cleared field instead of sending null', () => {
    for (const field of [
      'max_concurrent',
      'cpu_limit_cores',
      'memory_limit_mb',
      'build_cache_max_unused_days',
    ] as const) {
      expect(issueFor(field, Number.NaN)).toBeDefined()
    }
  })

  it('keeps the server ranges', () => {
    expect(issueFor('build_cache_max_gb', 10001)).toBe(
      'Must be between 0 and 10000'
    )
    expect(issueFor('build_cache_max_gb', 1.5)).toBe('Use a whole number of GB')
    expect(issueFor('build_cache_max_unused_days', 0)).toBe(
      'Must be between 1 and 365'
    )
    expect(issueFor('max_concurrent', 33)).toBe('Must be between 1 and 32')
    expect(issueFor('cpu_limit_cores', 0.5)).toBeUndefined()
  })
})
