// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { rolePriorityError } from './oidc-role-priority'

test('an empty box means the suggested priority and is accepted', () => {
  expect(rolePriorityError(undefined)).toBeNull()
})

test('whole numbers, including zero and negatives, are accepted', () => {
  expect(rolePriorityError(0)).toBeNull()
  expect(rolePriorityError(10)).toBeNull()
  expect(rolePriorityError(-1)).toBeNull()
})

test('a fraction is rejected with the typed value in the message', () => {
  expect(rolePriorityError(1.5)).toBe(
    'Priority must be a whole number (got 1.5)'
  )
})
