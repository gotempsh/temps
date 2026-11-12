// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { HealthcheckTimings } from '@/api/client/types.gen'
import {
  applyNowCopy,
  checksPerMinute,
  formatHealthcheckValue,
  formValuesOf,
  HEALTHCHECK_FIELDS,
  healthcheckFormSchema,
  overridesOf,
  type HealthcheckFormValues,
  type InheritedHealthcheck,
} from './service-healthcheck'

const REDIS: HealthcheckTimings = {
  interval_secs: 30,
  start_interval_secs: 2,
  start_period_secs: 90,
  timeout_secs: 5,
  retries: 3,
}
const MONGODB: HealthcheckTimings = { ...REDIS, timeout_secs: 10 }
const RUSTFS: HealthcheckTimings = { ...REDIS, start_period_secs: 60 }

const redis: InheritedHealthcheck = { timings: REDIS, readinessWaitSecs: 90 }
const mongodb: InheritedHealthcheck = {
  timings: MONGODB,
  readinessWaitSecs: 90,
}
const rustfs: InheritedHealthcheck = { timings: RUSTFS, readinessWaitSecs: 60 }

const EMPTY: HealthcheckFormValues = formValuesOf(undefined)

function issues(values: Partial<HealthcheckFormValues>, inherited = [redis]) {
  const result = healthcheckFormSchema(inherited).safeParse({
    ...EMPTY,
    ...values,
  })
  return result.success
    ? []
    : result.error.issues.map((i) => `${String(i.path[0])}: ${i.message}`)
}

describe('healthcheck form schema', () => {
  test('every field may be left empty to inherit', () => {
    expect(issues({})).toEqual([])
  })

  test('accepts each bound and refuses one past it, naming the field', () => {
    for (const spec of HEALTHCHECK_FIELDS) {
      // The interval's lower bound would otherwise trip the timeout check.
      const base: Partial<HealthcheckFormValues> =
        spec.field === 'interval_secs' ? { timeout_secs: '1' } : {}
      const timeoutBound =
        spec.field === 'timeout_secs' ? { interval_secs: '3600' } : {}
      for (const ok of [spec.min, spec.max]) {
        expect(
          issues({ ...base, ...timeoutBound, [spec.field]: String(ok) })
        ).toEqual([])
      }
      for (const bad of [spec.min - 1, spec.max + 1]) {
        if (bad < 0) continue
        const found = issues({
          ...base,
          ...timeoutBound,
          [spec.field]: String(bad),
        })
        expect(
          found.some((i) => i.startsWith(`${spec.field}: ${spec.label}`))
        ).toBe(true)
      }
    }
  })

  test('refuses something that is not a whole number', () => {
    expect(issues({ retries: '2.5' })).toEqual([
      'retries: Retries must be a whole number',
    ])
    expect(issues({ interval_secs: 'often' })[0]).toContain(
      'Interval must be a whole number'
    )
  })

  test('the timeout must stay shorter than the interval, inherited or not', () => {
    expect(issues({ interval_secs: '10', timeout_secs: '10' })).toEqual([
      'timeout_secs: Timeout (10s) must be shorter than the interval (10s)',
    ])
    // An interval below the inherited 5s timeout is blamed on the interval.
    expect(issues({ interval_secs: '5' })).toEqual([
      'interval_secs: Timeout (5s) must be shorter than the interval (5s)',
    ])
    // The instance default applies to every engine: 8s is fine for Redis
    // and too short for MongoDB's 10s timeout.
    expect(issues({ interval_secs: '8' }, [redis])).toEqual([])
    expect(issues({ interval_secs: '8' }, [redis, mongodb])).toEqual([
      'interval_secs: Timeout (10s) must be shorter than the interval (8s)',
    ])
  })
})

describe('a check must land inside the readiness wait', () => {
  test('a start period of 0 with a long interval is refused before Save', () => {
    expect(
      issues({ start_period_secs: '0', interval_secs: '120' }, [redis])
    ).toEqual([
      'start_period_secs: Start period (0s) plus interval (120s) must fit in the 90s Temps waits for a new container to become healthy, or the start period must cover it',
    ])
  })

  test('the start period covering the wait, or one interval fitting in it, is fine', () => {
    expect(issues({ interval_secs: '3600' }, [redis])).toEqual([])
    expect(
      issues({ start_period_secs: '0', interval_secs: '60' }, [redis])
    ).toEqual([])
    // Checking as often as while starting needs no start period.
    expect(
      issues(
        {
          start_period_secs: '0',
          start_interval_secs: '60',
          interval_secs: '60',
        },
        [rustfs]
      )
    ).toEqual([])
  })

  test("an instance default is checked against every engine's wait", () => {
    // 30s + 45s fits Redis' 90s wait but not RustFS' 60s.
    expect(
      issues({ start_period_secs: '30', interval_secs: '45' }, [redis])
    ).toEqual([])
    expect(
      issues({ start_period_secs: '30', interval_secs: '45' }, [redis, rustfs])
    ).toEqual([
      'start_period_secs: Start period (30s) plus interval (45s) must fit in the 60s Temps waits for a new container to become healthy, or the start period must cover it',
    ])
  })

  test('an inherited start period is blamed on the interval', () => {
    const noStartPeriod: InheritedHealthcheck = {
      timings: { ...REDIS, start_period_secs: 0 },
      readinessWaitSecs: 90,
    }
    expect(issues({ interval_secs: '120' }, [noStartPeriod])).toEqual([
      'interval_secs: Start period (0s) plus interval (120s) must fit in the 90s Temps waits for a new container to become healthy, or the start period must cover it',
    ])
  })
})

describe('healthcheck form values', () => {
  test('empty fields are sent as null and numbers as numbers', () => {
    expect(
      overridesOf({ ...EMPTY, interval_secs: ' 60 ', retries: '4' })
    ).toEqual({
      interval_secs: 60,
      start_interval_secs: null,
      start_period_secs: null,
      timeout_secs: null,
      retries: 4,
    })
  })

  test('saved overrides fill the form and read back unchanged', () => {
    const saved = {
      interval_secs: 120,
      start_interval_secs: null,
      start_period_secs: 0,
      timeout_secs: null,
      retries: null,
    }
    expect(formValuesOf(saved).start_period_secs).toBe('0')
    expect(overridesOf(formValuesOf(saved))).toEqual(saved)
  })
})

test('values read as durations and checks per minute', () => {
  expect(formatHealthcheckValue('interval_secs', 30)).toBe('30s')
  expect(formatHealthcheckValue('start_period_secs', 120)).toBe('2m')
  expect(formatHealthcheckValue('retries', 3)).toBe('3')
  expect(checksPerMinute(30)).toBe('2')
  expect(checksPerMinute(1)).toBe('60')
  expect(checksPerMinute(45)).toBe('1.3')
})

test('the Apply now confirmation names the service and the restart', () => {
  const copy = applyNowCopy('orders-db')
  expect(copy.title).toBe('Recreate orders-db now?')
  expect(copy.description).toContain('restarts briefly')
  expect(copy.description).toContain('data volumes and configuration are kept')
})
