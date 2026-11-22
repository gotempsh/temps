// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ErrorGroupActivityResponse } from '../../api/types.gen.js'
import { MAX_COMMENT_CHARS, checkCommentBody, describeActivityEntry } from './activity.js'

function entry(overrides: Partial<ErrorGroupActivityResponse>): ErrorGroupActivityResponse {
  return {
    id: 1,
    group_id: 5,
    kind: 'status_changed',
    actor_user_id: 2,
    actor_name: 'Ana',
    body: null,
    details: {},
    created_at: '2026-11-19T10:00:00Z',
    edited_at: null,
    ...overrides,
  }
}

describe('checkCommentBody', () => {
  test('trims and refuses empty or oversized comments', () => {
    expect(checkCommentBody('  rolled back  ')).toBe('rolled back')
    expect(() => checkCommentBody(undefined)).toThrow(/needs text/)
    expect(() => checkCommentBody('   ')).toThrow(/needs text/)
    expect(() => checkCommentBody('x'.repeat(MAX_COMMENT_CHARS + 1))).toThrow(/at most/)
  })
})

describe('describeActivityEntry', () => {
  test('describes status changes, regressions and assignments', () => {
    expect(describeActivityEntry(entry({ details: { status: 'resolved', in_next_release: true } }))).toBe(
      'set status resolved in the next release',
    )
    expect(
      describeActivityEntry(entry({ kind: 'regressed', details: { status: 'ignored', release: '3.1.0' } })),
    ).toBe('reopened by a new event in release 3.1.0 (was ignored)')
    expect(
      describeActivityEntry(
        entry({ kind: 'assigned', details: { assignee: { type: 'user', id: 9 }, assignee_name: 'Bo' } }),
      ),
    ).toBe('assigned to Bo')
    expect(describeActivityEntry(entry({ kind: 'assigned', details: {} }))).toBe('unassigned')
  })

  test('describes merges with the other groups and the fingerprints moved', () => {
    expect(describeActivityEntry(entry({ kind: 'merged', details: { group_ids: [3, 4], fingerprints: 2 } }))).toBe(
      'merged #3, #4 into this group (2 fingerprints)',
    )
  })
})
