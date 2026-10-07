// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { requestLogsRedirectPath } from './analytics-legacy-routes'

describe('legacy analytics request-log links', () => {
  test('the list redirects to request logs', () => {
    expect(requestLogsRedirectPath('web', '', '')).toBe(
      '/projects/web/request-logs'
    )
  })

  test('a log detail keeps its id and query string', () => {
    expect(requestLogsRedirectPath('web', '42', '?environment=3')).toBe(
      '/projects/web/request-logs/42?environment=3'
    )
  })

  test('does not produce a double slash', () => {
    expect(requestLogsRedirectPath('web', '/42', '')).toBe(
      '/projects/web/request-logs/42'
    )
  })
})
