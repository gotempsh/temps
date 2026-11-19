// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { notCheckedFromProblem, WAL_HEALTH_NOT_CHECKED_TYPE } from './wal-health'

describe('notCheckedFromProblem', () => {
  test('reads the reason of a not-checked 404', () => {
    expect(
      notCheckedFromProblem({
        type: WAL_HEALTH_NOT_CHECKED_TYPE,
        status: 404,
        detail: 'detail text',
        reason: 'the agent on node worker-3 does not support the WAL probe',
        checked_at: '2026-01-01T00:00:00Z',
      })
    ).toEqual({
      reason: 'the agent on node worker-3 does not support the WAL probe',
      checked_at: '2026-01-01T00:00:00Z',
    })
  })

  test('a plain 404 (no snapshot yet) is not a not-checked state', () => {
    expect(
      notCheckedFromProblem({ status: 404, detail: 'No WAL health snapshot' })
    ).toBeNull()
    expect(notCheckedFromProblem(undefined)).toBeNull()
  })
})
