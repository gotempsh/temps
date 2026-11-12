// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { formatUpdatedAgo, queryKeyPrefix } from './refresh'

describe('formatUpdatedAgo', () => {
  const at = Date.UTC(2026, 0, 1)
  test.each([
    [0, 'Updated just now'],
    [59_000, 'Updated just now'],
    [2 * 60_000, 'Updated 2m ago'],
    [3 * 3_600_000, 'Updated 3h ago'],
    [2 * 86_400_000, 'Updated 2d ago'],
    [-5_000, 'Updated just now'],
  ])('%i ms later reads %s', (elapsed, text) => {
    expect(formatUpdatedAgo(at, at + elapsed)).toBe(text)
  })
})

describe('queryKeyPrefix', () => {
  const listLogsQueryKey = (options: {
    path: { project_id: number }
    query?: { page?: number; status?: string }
  }) => [{ _id: 'listLogs', baseUrl: '', ...options }] as const

  test('builds the key from the scope it is given and nothing else', () => {
    expect(queryKeyPrefix(listLogsQueryKey)).toEqual([
      { _id: 'listLogs', baseUrl: '' },
    ])
    expect(
      queryKeyPrefix(listLogsQueryKey, { path: { project_id: 7 } })
    ).toEqual([{ _id: 'listLogs', baseUrl: '', path: { project_id: 7 } }])
  })
})
