// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MalformedGroupListError,
  shouldRetryGroupList,
} from './useProjectGroups'

describe('shouldRetryGroupList', () => {
  test('never retries a body that is not a page of groups', () => {
    expect(shouldRetryGroupList(0, new MalformedGroupListError(1))).toBe(false)
  })

  test('keeps the shared policy for transport failures', () => {
    expect(shouldRetryGroupList(0, new TypeError('Failed to fetch'))).toBe(true)
    expect(shouldRetryGroupList(3, new TypeError('Failed to fetch'))).toBe(
      false
    )
  })

  test('does not retry a refused request', () => {
    expect(shouldRetryGroupList(0, { status: 403, title: 'Forbidden' })).toBe(
      false
    )
  })
})
