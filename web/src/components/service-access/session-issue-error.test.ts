// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  isServiceAccessUnavailable,
  SERVICE_ACCESS_UNAVAILABLE_TITLE,
} from './session-issue-error'

test('only the service-access-unavailable problem is treated as unrecoverable', () => {
  expect(
    isServiceAccessUnavailable({ title: SERVICE_ACCESS_UNAVAILABLE_TITLE })
  ).toBe(true)
  expect(isServiceAccessUnavailable({ title: 'Conflict' })).toBe(false)
  expect(isServiceAccessUnavailable(new Error('boom'))).toBe(false)
  expect(isServiceAccessUnavailable(undefined)).toBe(false)
})
