// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'

import { blankRequiredField } from './blank-required-field'

test('names the first field that holds only whitespace', () => {
  expect(
    blankRequiredField([
      ['Name', '   '],
      ['Slug', ''],
    ])
  ).toBe('Name is required and cannot be only spaces')
  expect(
    blankRequiredField([
      ['Name', 'Code review'],
      ['Slug', ''],
    ])
  ).toBe('Slug is required and cannot be only spaces')
})

test('accepts filled fields', () => {
  expect(blankRequiredField([['Name', ' Code review ']])).toBeNull()
})
