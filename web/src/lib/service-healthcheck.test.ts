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

describe('a check must be seen inside the readiness wait', () => {
  // RustFS: a 60s wait and a 5s timeout, so a check has to start by
  // 60 - 5 - READINESS_POLL_SECS = 53s for Temps to read it in time.
  test('without a start period, a first check that would end after the wait is refused before Save', () => {
    expect(
      issues({ start_period_secs: '0', interval_secs: '120' }, [redis])
    ).toEqual([
      'interval_secs: The first check comes 120s in and may take 5s, so it would end after the 90s Temps waits for a new container to become healthy; it has to start by 83s',
    ])
    // Starting exactly as the wait ends is still too late.
    expect(
      issues({ start_period_secs: '0', interval_secs: '60' }, [rustfs])[0]
    ).toStartWith('interval_secs: The first check comes 60s in')
    expect(
      issues({ start_period_secs: '0', interval_secs: '54' }, [rustfs]).length
    ).toBe(1)
    expect(
      issues({ start_period_secs: '0', interval_secs: '53' }, [rustfs])
    ).toEqual([])
  })

  test('with a start period, the first check comes one start interval in', () => {
    expect(
      issues(
        {
          start_period_secs: '30',
          start_interval_secs: '60',
          interval_secs: '60',
        },
        [rustfs]
      )
    ).toEqual([
      'start_interval_secs: The first check comes 60s in and may take 5s, so it would end after the 60s Temps waits for a new container to become healthy; it has to start by 53s',
    ])
  })

  test('a slower check needs its own timeout inside the wait', () => {
    // MongoDB: a 90s wait and a 10s timeout.
    expect(
      issues({ start_period_secs: '0', interval_secs: '90' }, [mongodb])[0]
    ).toStartWith(
      'interval_secs: The first check comes 90s in and may take 10s'
    )
    expect(
      issues({ start_period_secs: '0', interval_secs: '79' }, [mongodb]).length
    ).toBe(1)
    expect(
      issues({ start_period_secs: '0', interval_secs: '78' }, [mongodb])
    ).toEqual([])
  })

  test('the start period lasting until the last useful check, or one interval fitting before it, is fine', () => {
    expect(issues({ interval_secs: '3600' }, [redis])).toEqual([])
    expect(
      issues({ start_period_secs: '55', interval_secs: '10' }, [rustfs])
    ).toEqual([])
    // 20s, the last check inside it starting 2s after it ends and taking 5s,
    // then 26s: the next starts at 53s.
    expect(
      issues({ start_period_secs: '20', interval_secs: '26' }, [rustfs])
    ).toEqual([])
    expect(
      issues({ start_period_secs: '20', interval_secs: '27' }, [rustfs])[0]
    ).toStartWith('start_period_secs: Start period (20s), the last check')
    // Checking as often as while starting needs no start period.
    expect(
      issues(
        {
          start_period_secs: '0',
          start_interval_secs: '10',
          interval_secs: '10',
        },
        [rustfs]
      )
    ).toEqual([])
  })

  test("an instance default is checked against every engine's wait", () => {
    // 30s + 5s + 30s leaves time inside Redis' 90s wait but not RustFS' 60s.
    expect(
      issues({ start_period_secs: '30', interval_secs: '30' }, [redis])
    ).toEqual([])
    expect(
      issues({ start_period_secs: '30', interval_secs: '30' }, [redis, rustfs])
    ).toEqual([
      'start_period_secs: Start period (30s), the last check scheduled inside it (starting up to 2s after it ends and taking up to 5s) and one interval (30s) must leave the next 5s check time to finish inside the 60s Temps waits for a new container to become healthy (by 53s), or the start period must last that long',
    ])
  })

  test('a start interval longer than the start period delays the next check', () => {
    // PostgreSQL waits 90s: the first check comes 30s in, after the 10s start
    // period, and the next one 5s + 60s later, at 95s.
    const postgres: InheritedHealthcheck = {
      timings: { ...REDIS, start_period_secs: 120 },
      readinessWaitSecs: 90,
    }
    const values = (startInterval: string) => ({
      start_period_secs: '10',
      start_interval_secs: startInterval,
      interval_secs: '60',
    })
    expect(issues(values('30'), [postgres])).toEqual([
      'start_period_secs: Start period (10s), the last check scheduled inside it (starting up to 30s after it ends and taking up to 5s) and one interval (60s) must leave the next 5s check time to finish inside the 90s Temps waits for a new container to become healthy (by 83s), or the start period must last that long',
    ])
    // 10s + 8s + 5s + 60s: the next check starts at 83s, just in time.
    expect(issues(values('8'), [postgres])).toEqual([])
  })

  test('an inherited start period is blamed on the interval', () => {
    const shortStart: InheritedHealthcheck = {
      timings: { ...REDIS, start_period_secs: 10 },
      readinessWaitSecs: 90,
    }
    expect(issues({ interval_secs: '120' }, [shortStart])).toEqual([
      'interval_secs: Start period (10s), the last check scheduled inside it (starting up to 2s after it ends and taking up to 5s) and one interval (120s) must leave the next 5s check time to finish inside the 90s Temps waits for a new container to become healthy (by 83s), or the start period must last that long',
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
