// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

import {
  stopTimeoutProblem,
  stopTimeoutSeconds,
} from './deployment-stop-timeout'

test('accepts blank and whole seconds from 1 to 3600', () => {
  for (const value of ['', '  ', '1', '120', '3600']) {
    expect([value, stopTimeoutProblem(value)]).toEqual([value, null])
  }
})

test('refuses what the server would refuse', () => {
  for (const value of ['0', '3601', '1.5', '-5', 'ten']) {
    expect([value, stopTimeoutProblem(value)]).toEqual([
      value,
      'Enter a whole number of seconds between 1 and 3600',
    ])
  }
})

test('sends blank as null so the environment follows the project', () => {
  expect(stopTimeoutSeconds('')).toBeNull()
  expect(stopTimeoutSeconds(' 90 ')).toBe(90)
})
