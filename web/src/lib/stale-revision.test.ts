// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { isStaleRevision } from './stale-revision'

test('only a refusal carrying the stored revision is a stale save', () => {
  expect(
    isStaleRevision({
      title: 'Settings Changed Since They Were Read',
      expected_revision: '1',
      current_revision: '2',
    })
  ).toBe(true)
  expect(isStaleRevision({ title: 'Worker Does Not Allow This Change' })).toBe(
    false
  )
  expect(isStaleRevision(new Error('network'))).toBe(false)
  expect(isStaleRevision(undefined)).toBe(false)
})
