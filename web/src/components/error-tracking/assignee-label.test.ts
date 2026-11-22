// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { assigneeInitials, assigneeLabel } from './assignee-label'

test('the label names a user, marks a team, and falls back to the legacy text', () => {
  expect(
    assigneeLabel({
      type: 'user',
      id: 1,
      name: 'Dana Ruiz',
      email: 'd@example.test',
    })
  ).toBe('Dana Ruiz')
  expect(
    assigneeLabel({ type: 'team', id: 2, name: 'Checkout', email: null })
  ).toBe('Checkout (team)')
  expect(assigneeLabel(null, 'someone outside temps')).toBe(
    'someone outside temps'
  )
  expect(assigneeLabel(undefined)).toBe('Unassigned')
})

test('initials take the first letters of two words, or two letters of one', () => {
  expect(
    assigneeInitials({ type: 'user', id: 1, name: 'dana ruiz', email: null })
  ).toBe('DR')
  expect(
    assigneeInitials({ type: 'user', id: 1, name: 'eli', email: null })
  ).toBe('EL')
})
