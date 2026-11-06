// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { z } from 'zod'

import {
  refineTriggerConfig,
  triggerConfigForRequest,
  triggerConfigFromRule,
  triggerConfigSchema,
} from './alert-rule-trigger-config'

const ruleSchema = z
  .object({ trigger_type: z.string(), trigger_config: triggerConfigSchema })
  .superRefine(refineTriggerConfig)

function issues(rule: z.input<typeof ruleSchema>) {
  const result = ruleSchema.safeParse(rule)
  return result.success
    ? []
    : result.error.issues.map((i) => [i.path.join('.'), i.message])
}

describe('user count rules', () => {
  test('are saved with the count the server reads', () => {
    expect(triggerConfigForRequest('user_count', { threshold: 25 })).toEqual({
      count: 25,
    })
  })

  test('read back a count, or a threshold an older console saved', () => {
    expect(triggerConfigFromRule('user_count', { count: 25 })).toEqual({
      threshold: 25,
    })
    expect(triggerConfigFromRule('user_count', { threshold: 7 })).toEqual({
      threshold: 7,
    })
  })

  test('cannot be saved without a user threshold', () => {
    expect(issues({ trigger_type: 'user_count', trigger_config: {} })).toEqual([
      ['trigger_config.threshold', 'User threshold is required'],
    ])
    expect(
      issues({ trigger_type: 'user_count', trigger_config: { threshold: 0 } })
    ).toEqual([
      ['trigger_config.threshold', 'User threshold must be at least 1'],
    ])
  })
})

describe('frequency rules', () => {
  test('need an event count and a time window', () => {
    expect(issues({ trigger_type: 'frequency', trigger_config: {} })).toEqual([
      ['trigger_config.count', 'Event count is required'],
      ['trigger_config.window_minutes', 'Time window is required'],
    ])
  })

  test('are saved with both fields', () => {
    expect(
      triggerConfigForRequest('frequency', { count: 100, window_minutes: 60 })
    ).toEqual({ count: 100, window_minutes: 60 })
    expect(
      issues({
        trigger_type: 'frequency',
        trigger_config: { count: 100, window_minutes: 60 },
      })
    ).toEqual([])
  })
})

test('other triggers send no configuration and need none', () => {
  expect(triggerConfigForRequest('new_issue', { count: 3 })).toBeUndefined()
  expect(issues({ trigger_type: 'new_issue' })).toEqual([])
})
