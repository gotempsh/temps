// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { logEnvironmentLabel } from './log-environment'

test('environment IDs display their slug without changing the filter value', () => {
  expect(logEnvironmentLabel('2', { '2': 'production' })).toBe('production')
  expect(logEnvironmentLabel('production', {})).toBe('production')
  expect(logEnvironmentLabel('2', {})).toBe('Unknown environment')
})
