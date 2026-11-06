// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The trigger configuration of an error alert rule, as the form edits it and
 * as the server stores it.
 *
 * A user count rule is stored as `{ count }`. Consoles before #1749 saved it as
 * `{ threshold }`, which the server never read, so those rules never fired; the
 * server now accepts both, and the form reads either and writes `count`.
 */

import { z } from 'zod'

export interface TriggerConfigFields {
  /** Frequency: events within the window. */
  count?: number
  window_minutes?: number
  /** User count: unique users affected. Stored as `count`. */
  threshold?: number
}

export function needsTriggerConfig(triggerType: string): boolean {
  return triggerType === 'frequency' || triggerType === 'user_count'
}

export function triggerConfigFromRule(
  triggerType: string,
  stored: Record<string, unknown> | null | undefined
): TriggerConfigFields {
  const config = stored ?? {}
  const number = (value: unknown) =>
    typeof value === 'number' ? value : undefined
  if (triggerType === 'user_count') {
    return { threshold: number(config.count) ?? number(config.threshold) }
  }
  return {
    count: number(config.count),
    window_minutes: number(config.window_minutes),
  }
}

export function triggerConfigForRequest(
  triggerType: string,
  fields: TriggerConfigFields | undefined
): Record<string, number> | undefined {
  if (triggerType === 'frequency') {
    return {
      count: fields?.count ?? 0,
      window_minutes: fields?.window_minutes ?? 0,
    }
  }
  if (triggerType === 'user_count') {
    return { count: fields?.threshold ?? 0 }
  }
  return undefined
}

const positiveWholeNumber = (label: string) =>
  z
    .number({ message: `${label} is required` })
    .int(`${label} must be a whole number`)
    .min(1, `${label} must be at least 1`)

export const triggerConfigSchema = z
  .object({
    count: z.number().optional(),
    window_minutes: z.number().optional(),
    threshold: z.number().optional(),
  })
  .optional()

/**
 * Adds the per-trigger requirements to a rule form: a frequency rule needs an
 * event count and a window, a user count rule needs a user threshold. Without
 * them the server refuses the rule (or, before #1749, saved one that never
 * fired).
 */
export function refineTriggerConfig(
  rule: { trigger_type: string; trigger_config?: TriggerConfigFields },
  ctx: z.RefinementCtx
): void {
  const config = rule.trigger_config ?? {}
  const require = (field: keyof TriggerConfigFields, label: string): void => {
    const result = positiveWholeNumber(label).safeParse(config[field])
    if (!result.success) {
      ctx.addIssue({
        code: 'custom',
        path: ['trigger_config', field],
        message: result.error.issues[0]?.message ?? `${label} is required`,
      })
    }
  }
  if (rule.trigger_type === 'frequency') {
    require('count', 'Event count')
    require('window_minutes', 'Time window')
  } else if (rule.trigger_type === 'user_count') {
    require('threshold', 'User threshold')
  }
}
