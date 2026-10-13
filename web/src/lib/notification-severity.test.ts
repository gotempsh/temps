// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { errorRulePrioritySeverity } from './notification-severity'

test('error rule priorities map to the severity routes match on', () => {
  expect(errorRulePrioritySeverity('Low')).toBe('info')
  expect(errorRulePrioritySeverity('Normal')).toBe('warning')
  expect(errorRulePrioritySeverity('High')).toBe('error')
  expect(errorRulePrioritySeverity('Critical')).toBe('critical')
  expect(errorRulePrioritySeverity('unexpected')).toBe('error')
})
