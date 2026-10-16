// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  budgetUsedPercent,
  describeInstanceBudget,
  describeInstanceRequestLimit,
  describeLimitSource,
  formatBudgetLimit,
  formatEffectiveBudget,
  formatEffectiveRequestLimit,
  formatMicrocents,
  formatRequestLimit,
  limitFormSchema,
  limitsToFormValues,
  MAX_REQUESTS_PER_MINUTE,
  monthStartIso,
  parseLimitForm,
  usdToMicrocents,
} from './ai-gateway-limits'

describe('formatting', () => {
  test('null inherits, zero is unlimited, positive is a cap', () => {
    expect(formatRequestLimit(null)).toBe('Inherited')
    expect(formatRequestLimit(undefined)).toBe('Inherited')
    expect(formatRequestLimit(0)).toBe('Unlimited')
    expect(formatRequestLimit(1500)).toBe('1,500 / min')
    expect(formatBudgetLimit(null)).toBe('Inherited')
    expect(formatBudgetLimit(0)).toBe('Unlimited')
    expect(formatBudgetLimit(usdToMicrocents(25))).toBe('$25.00 / month')
  })

  test('effective values treat null as no limit', () => {
    expect(formatEffectiveRequestLimit(null)).toBe('Unlimited')
    expect(formatEffectiveRequestLimit(300)).toBe('300 / min')
    expect(formatEffectiveBudget(null)).toBe('Unlimited')
    expect(formatEffectiveBudget(usdToMicrocents(1.5))).toBe('$1.50 / month')
  })

  test('sub-cent spend keeps precision', () => {
    expect(formatMicrocents(0)).toBe('$0.00')
    expect(formatMicrocents(usdToMicrocents(0.0042))).toBe('$0.0042')
    expect(formatMicrocents(usdToMicrocents(12.3456))).toBe('$12.35')
  })

  test('source labels', () => {
    expect(describeLimitSource('project')).toBe('Project override')
    expect(describeLimitSource('instance')).toBe('Instance default')
    expect(describeLimitSource('built_in')).toBe('Built-in default')
  })
})

describe('instance scope display', () => {
  test('unset request limit shows the built-in default', () => {
    expect(describeInstanceRequestLimit(null, 300)).toEqual({
      value: '300 / min',
      note: 'Built-in default',
    })
    expect(describeInstanceRequestLimit(0, 300).value).toBe('Unlimited')
    expect(describeInstanceRequestLimit(50, 300).value).toBe('50 / min')
  })

  test('unset budget is no cap', () => {
    expect(describeInstanceBudget(null)).toEqual({
      value: 'Unlimited',
      note: 'No budget set',
    })
    expect(describeInstanceBudget(0).value).toBe('Unlimited')
    expect(describeInstanceBudget(usdToMicrocents(10)).value).toBe(
      '$10.00 / month'
    )
  })
})

describe('budgetUsedPercent', () => {
  test('is null without a positive budget', () => {
    expect(budgetUsedPercent(100, null)).toBeNull()
    expect(budgetUsedPercent(100, 0)).toBeNull()
  })

  test('clamps to 0-100', () => {
    expect(budgetUsedPercent(50, 200)).toBe(25)
    expect(budgetUsedPercent(500, 200)).toBe(100)
    expect(budgetUsedPercent(-5, 200)).toBe(0)
  })
})

describe('parseLimitForm', () => {
  test('empty fields inherit', () => {
    const result = parseLimitForm({
      requestsPerMinute: '',
      monthlyBudgetUsd: ' ',
    })
    expect(result).toEqual({
      ok: true,
      payload: {
        max_requests_per_minute: null,
        max_cost_per_month_microcents: null,
      },
    })
  })

  test('zero is preserved as unlimited, not dropped as falsy', () => {
    const result = parseLimitForm({
      requestsPerMinute: '0',
      monthlyBudgetUsd: '0',
    })
    expect(result).toEqual({
      ok: true,
      payload: {
        max_requests_per_minute: 0,
        max_cost_per_month_microcents: 0,
      },
    })
  })

  test('converts dollars to microcents', () => {
    const result = parseLimitForm({
      requestsPerMinute: '120',
      monthlyBudgetUsd: '12.5',
    })
    expect(result).toEqual({
      ok: true,
      payload: {
        max_requests_per_minute: 120,
        max_cost_per_month_microcents: 12_500_000,
      },
    })
  })

  test('rejects fractions, negatives, junk and over-limit values', () => {
    const bad = parseLimitForm({
      requestsPerMinute: '1.5',
      monthlyBudgetUsd: '-1',
    })
    expect(bad.ok).toBe(false)
    if (!bad.ok) {
      expect(bad.errors.requestsPerMinute).toBeDefined()
      expect(bad.errors.monthlyBudgetUsd).toBeDefined()
    }

    const junk = parseLimitForm({
      requestsPerMinute: 'abc',
      monthlyBudgetUsd: 'abc',
    })
    expect(junk.ok).toBe(false)

    const over = parseLimitForm({
      requestsPerMinute: String(MAX_REQUESTS_PER_MINUTE + 1),
      monthlyBudgetUsd: '',
    })
    expect(over.ok).toBe(false)
    expect(
      parseLimitForm({
        requestsPerMinute: String(MAX_REQUESTS_PER_MINUTE),
        monthlyBudgetUsd: '',
      }).ok
    ).toBe(true)
  })
})

describe('limitsToFormValues', () => {
  test('round-trips through parseLimitForm', () => {
    const stored = {
      max_requests_per_minute: 60,
      max_cost_per_month_microcents: usdToMicrocents(40),
    }
    const form = limitsToFormValues(stored)
    expect(form).toEqual({ requestsPerMinute: '60', monthlyBudgetUsd: '40' })
    const parsed = parseLimitForm(form)
    expect(parsed.ok && parsed.payload).toEqual(stored)
  })

  test('null fields become empty inputs', () => {
    expect(
      limitsToFormValues({
        max_requests_per_minute: null,
        max_cost_per_month_microcents: null,
      })
    ).toEqual({ requestsPerMinute: '', monthlyBudgetUsd: '' })
  })
})

describe('limitFormSchema', () => {
  test('reports field errors on the matching paths', () => {
    const result = limitFormSchema.safeParse({
      requestsPerMinute: '-3',
      monthlyBudgetUsd: 'x',
    })
    expect(result.success).toBe(false)
    if (!result.success) {
      const paths = result.error.issues.map((i) => i.path[0])
      expect(paths).toContain('requestsPerMinute')
      expect(paths).toContain('monthlyBudgetUsd')
    }
  })

  test('accepts blank (inherit) values', () => {
    expect(
      limitFormSchema.safeParse({ requestsPerMinute: '', monthlyBudgetUsd: '' })
        .success
    ).toBe(true)
  })
})

describe('monthStartIso', () => {
  test('uses the UTC month, not the local one', () => {
    expect(monthStartIso(new Date('2026-10-31T23:59:59Z'))).toBe(
      '2026-10-01T00:00:00.000Z'
    )
    expect(monthStartIso(new Date('2026-01-01T00:00:00Z'))).toBe(
      '2026-01-01T00:00:00.000Z'
    )
  })
})
