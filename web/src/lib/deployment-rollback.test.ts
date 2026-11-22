// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import {
  canRollbackTo,
  deploymentMenuHasActions,
  ROLLBACK_TARGET_STATUSES,
} from './deployment-rollback'

// [status, is_current, expected]. The accepted states mirror the server's
// rollback validation; the current deployment is never offered.
const cases: Array<[string, boolean, boolean]> = [
  ['stopped', false, true],
  ['completed', false, true],
  ['deployed', false, true],
  ['completed', true, false],
  ['stopped', true, false],
  ['deployed', true, false],
  ['failed', false, false],
  ['cancelled', false, false],
  ['paused', false, false],
  ['pending', false, false],
  ['running', false, false],
  ['building', false, false],
  ['built', false, false],
  ['superseded', false, false],
  ['', false, false],
]

test.each(cases)(
  'canRollbackTo(status=%p, is_current=%p) is %p',
  (status, isCurrent, expected) => {
    expect(canRollbackTo({ status, is_current: isCurrent })).toBe(expected)
  }
)

test('a deployment without an is_current flag is treated as not current', () => {
  expect(canRollbackTo({ status: 'stopped' })).toBe(true)
})

test('the accepted statuses match the server list exactly', () => {
  expect([...ROLLBACK_TARGET_STATUSES].sort()).toEqual([
    'completed',
    'deployed',
    'stopped',
  ])
})

test('the detail menu is shown for a stopped previous deployment', () => {
  expect(
    deploymentMenuHasActions({ status: 'stopped', is_current: false })
  ).toBe(true)
  expect(deploymentMenuHasActions({ status: 'running' })).toBe(true)
  expect(deploymentMenuHasActions({ status: 'paused' })).toBe(true)
  expect(
    deploymentMenuHasActions({ status: 'completed', is_current: true })
  ).toBe(true)
})

test('the detail menu is hidden when no entry would apply', () => {
  for (const status of ['failed', 'cancelled', 'building']) {
    expect(deploymentMenuHasActions({ status, is_current: false })).toBe(false)
  }
  // A current stopped deployment has no rollback, pause, cancel or resume.
  expect(
    deploymentMenuHasActions({ status: 'stopped', is_current: true })
  ).toBe(false)
})
