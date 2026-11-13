// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  HealthcheckOverrides,
  HealthcheckSource,
  HealthcheckTimings,
  ManagedEngine,
} from '@/api/client/types.gen'

export type HealthcheckField = keyof HealthcheckTimings

export interface HealthcheckFieldSpec {
  field: HealthcheckField
  label: string
  min: number
  max: number
  /** `s` for a duration in seconds; empty for a count. */
  unit: 's' | ''
}

/** The fields in the order they are shown, with the server's bounds. */
export const HEALTHCHECK_FIELDS: readonly HealthcheckFieldSpec[] = [
  { field: 'interval_secs', label: 'Interval', min: 5, max: 3600, unit: 's' },
  {
    field: 'start_interval_secs',
    label: 'Start interval',
    min: 1,
    max: 60,
    unit: 's',
  },
  {
    field: 'start_period_secs',
    label: 'Start period',
    min: 0,
    max: 600,
    unit: 's',
  },
  { field: 'timeout_secs', label: 'Timeout', min: 1, max: 60, unit: 's' },
  { field: 'retries', label: 'Retries', min: 1, max: 10, unit: '' },
]

export const APPLY_ON_RECREATE =
  'Healthcheck changes apply when the container is recreated.'

const ENGINE_LABELS: Record<ManagedEngine, string> = {
  postgres: 'PostgreSQL',
  redis: 'Redis',
  mongodb: 'MongoDB',
  mariadb: 'MariaDB',
  rustfs: 'RustFS',
  minio: 'MinIO',
}

export function engineLabel(engine: ManagedEngine): string {
  return ENGINE_LABELS[engine] ?? engine
}

export function sourceLabel(source: HealthcheckSource): string {
  switch (source) {
    case 'service':
      return 'This service'
    case 'instance_default':
      return 'Instance default'
    case 'builtin':
      return 'Built-in'
  }
}

export function formatHealthcheckValue(
  field: HealthcheckField,
  value: number
): string {
  const spec = HEALTHCHECK_FIELDS.find((f) => f.field === field)
  if (spec?.unit !== 's') return String(value)
  if (value >= 60 && value % 60 === 0) return `${value / 60}m`
  return `${value}s`
}

/** One text input per field; empty inherits. */
export type HealthcheckFormValues = Record<HealthcheckField, string>

export function formValuesOf(
  overrides: HealthcheckOverrides | undefined
): HealthcheckFormValues {
  const value = (v: number | null | undefined) =>
    v === null || v === undefined ? '' : String(v)
  return {
    interval_secs: value(overrides?.interval_secs),
    start_interval_secs: value(overrides?.start_interval_secs),
    start_period_secs: value(overrides?.start_period_secs),
    timeout_secs: value(overrides?.timeout_secs),
    retries: value(overrides?.retries),
  }
}

export function overridesOf(
  values: HealthcheckFormValues
): HealthcheckOverrides {
  const value = (v: string) => (v.trim() === '' ? null : Number(v.trim()))
  return {
    interval_secs: value(values.interval_secs),
    start_interval_secs: value(values.start_interval_secs),
    start_period_secs: value(values.start_period_secs),
    timeout_secs: value(values.timeout_secs),
    retries: value(values.retries),
  }
}

function fieldSchema(spec: HealthcheckFieldSpec) {
  const unit = spec.unit === 's' ? ' seconds' : ''
  return z
    .string()
    .trim()
    .refine((v) => v === '' || /^\d+$/.test(v), {
      message: `${spec.label} must be a whole number`,
    })
    .refine(
      (v) => v === '' || (Number(v) >= spec.min && Number(v) <= spec.max),
      {
        message: `${spec.label} must be between ${spec.min} and ${spec.max}${unit}`,
      }
    )
}

/** What an empty field falls back to for one engine, and how long that
 * engine's readiness wait gives a new container to become healthy. */
export interface InheritedHealthcheck {
  timings: HealthcheckTimings
  readinessWaitSecs: number
}

/** The longest Temps' readiness waits take to read a container's health
 * once Docker has it (`READINESS_POLL_SECS` on the server). */
export const READINESS_POLL_SECS = 2

/**
 * The override form's schema. `inherited` is what an empty field falls back
 * to, one entry per engine the values apply to. For every one of them, as
 * the server checks: the timeout stays shorter than the interval, and a check
 * is seen inside the engine's readiness wait. A check has to start by the
 * wait minus the timeout and the poll: the first one (one start interval in
 * with a start period, one interval in without), and checks keep coming
 * until then (the start period lasts that long, the first check after it
 * fits, counting a check still running as it ends, or the interval is no
 * longer than the start interval).
 */
export function healthcheckFormSchema(inherited: InheritedHealthcheck[]) {
  const shape = Object.fromEntries(
    HEALTHCHECK_FIELDS.map((spec) => [spec.field, fieldSchema(spec)])
  ) as Record<HealthcheckField, ReturnType<typeof fieldSchema>>
  return z.object(shape).superRefine((values, ctx) => {
    const overrides = overridesOf(values)
    for (const { timings: base, readinessWaitSecs: wait } of inherited) {
      const interval = overrides.interval_secs ?? base.interval_secs
      const timeout = overrides.timeout_secs ?? base.timeout_secs
      const startPeriod = overrides.start_period_secs ?? base.start_period_secs
      const startInterval =
        overrides.start_interval_secs ?? base.start_interval_secs
      if ([interval, timeout, startPeriod, startInterval].some(Number.isNaN)) {
        return
      }
      if (timeout >= interval) {
        ctx.addIssue({
          code: 'custom',
          path:
            overrides.timeout_secs !== null
              ? ['timeout_secs']
              : ['interval_secs'],
          message: `Timeout (${timeout}s) must be shorter than the interval (${interval}s)`,
        })
        return
      }
      const lastStart = Math.max(0, wait - timeout - READINESS_POLL_SECS)
      const firstField: HealthcheckField =
        startPeriod > 0 ? 'start_interval_secs' : 'interval_secs'
      const firstCheck = startPeriod > 0 ? startInterval : interval
      if (firstCheck > lastStart) {
        ctx.addIssue({
          code: 'custom',
          path: [firstField],
          message: `The first check comes ${firstCheck}s in and may take ${timeout}s, so it would end after the ${wait}s Temps waits for a new container to become healthy; it has to start by ${lastStart}s`,
        })
        return
      }
      // Docker picks each wait as a check finishes, so the last check
      // scheduled inside the start period may start one start interval after
      // it ends and take its timeout; the next comes one interval later.
      const boundaryCheck = startPeriod > 0 ? startInterval + timeout : 0
      const covered =
        startPeriod >= lastStart ||
        startPeriod + boundaryCheck + interval <= lastStart ||
        interval <= startInterval
      if (!covered) {
        ctx.addIssue({
          code: 'custom',
          path:
            overrides.start_period_secs !== null
              ? ['start_period_secs']
              : ['interval_secs'],
          message: `Start period (${startPeriod}s), the last check scheduled inside it (starting up to ${startInterval}s after it ends and taking up to ${timeout}s) and one interval (${interval}s) must leave the next ${timeout}s check time to finish inside the ${wait}s Temps waits for a new container to become healthy (by ${lastStart}s), or the start period must last that long`,
        })
        return
      }
    }
  })
}

/** Healthcheck execs per minute for one container at this interval. */
export function checksPerMinute(intervalSecs: number): string {
  const perMinute = 60 / intervalSecs
  return Number.isInteger(perMinute) ? String(perMinute) : perMinute.toFixed(1)
}

export function applyNowCopy(serviceName: string) {
  return {
    title: `Recreate ${serviceName} now?`,
    description: `${serviceName} restarts briefly while its container is recreated with the new healthcheck. Its data volumes and configuration are kept.`,
    confirmLabel: 'Recreate container',
  }
}
