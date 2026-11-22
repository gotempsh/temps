// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  activityErrorMessage,
  activityGate,
  formatElapsed,
  isActionableFailure,
  secondsUntil,
} from './activity-request-state'

const noCooldowns = {
  website_analysis_secs: null,
  preview_secs: null,
  report_secs: null,
}

test('a cooldown from the status counts down from when the status was read', () => {
  const read = 1_000_000
  const gate = activityGate(
    {
      in_progress: null,
      cooldowns: { ...noCooldowns, website_analysis_secs: 30 },
    },
    'website_analysis',
    read
  )
  expect(gate.running).toBeNull()
  expect(gate.cooldownUntil).toBe(read + 30_000)
  expect(secondsUntil(gate.cooldownUntil, read + 12_400)).toBe(18)
  expect(secondsUntil(gate.cooldownUntil, read + 31_000)).toBe(0)
  // Another action's cooldown does not block this one.
  expect(
    activityGate(
      { in_progress: null, cooldowns: { ...noCooldowns, preview_secs: 30 } },
      'website_analysis',
      read
    ).cooldownUntil
  ).toBeNull()
})

test('any running action blocks the others, with its start time', () => {
  const gate = activityGate(
    {
      in_progress: { operation: 'report', started_at: '2026-11-22T10:00:00Z' },
      cooldowns: noCooldowns,
    },
    'website_analysis',
    0
  )
  expect(gate.running).toEqual({
    operation: 'report',
    startedAt: Date.parse('2026-11-22T10:00:00Z'),
  })
})

test('running and cooling-down errors are shown as state, provider errors as failures', () => {
  expect(isActionableFailure({ code: 'analysis_running' })).toBe(false)
  expect(isActionableFailure({ code: 'cooling_down' })).toBe(false)
  expect(isActionableFailure({ code: 'provider_error' })).toBe(true)
  expect(isActionableFailure(new Error('network'))).toBe(true)
  expect(
    activityErrorMessage({
      code: 'provider_error',
      detail:
        "The AI provider rejected the request: model 'mock-flash' not found",
    })
  ).toContain("model 'mock-flash' not found")
  expect(activityErrorMessage(undefined)).toContain('Check the AI provider')
})

test('elapsed time reads in seconds, then minutes', () => {
  expect(formatElapsed(0, 42_900)).toBe('42s')
  expect(formatElapsed(0, 125_000)).toBe('2m 5s')
})
