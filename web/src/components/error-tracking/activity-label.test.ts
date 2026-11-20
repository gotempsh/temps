// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { ErrorGroupActivityResponse } from '@/api/client/types.gen'
import {
  activityActor,
  commentPreview,
  describeActivity,
} from './activity-label'

function entry(
  overrides: Partial<ErrorGroupActivityResponse>
): ErrorGroupActivityResponse {
  return {
    id: 1,
    group_id: 7,
    kind: 'status_changed',
    actor_user_id: 3,
    actor_name: 'Ana',
    body: null,
    details: {},
    created_at: '2026-11-19T10:00:00Z',
    edited_at: null,
    ...overrides,
  }
}

describe('describeActivity', () => {
  it('names a status change, its next-release variant and a bulk action', () => {
    expect(describeActivity(entry({ details: { status: 'resolved' } }))).toBe(
      'resolved the group'
    )
    expect(
      describeActivity(
        entry({
          details: { status: 'resolved', in_next_release: true, bulk: true },
        })
      )
    ).toBe('resolved the group in the next release (bulk action)')
    expect(describeActivity(entry({ details: { status: 'unresolved' } }))).toBe(
      'reopened the group'
    )
  })

  it('describes a regression with its release and no actor', () => {
    const regressed = entry({
      kind: 'regressed',
      actor_user_id: null,
      actor_name: null,
      details: { status: 'resolved', release: '2.4.1' },
    })
    expect(describeActivity(regressed)).toBe(
      'A new event in release 2.4.1 reopened this resolved group'
    )
    expect(activityActor(regressed)).toBeNull()
  })

  it('describes merges and splits with the other groups', () => {
    expect(
      describeActivity(
        entry({
          kind: 'merged',
          details: { group_ids: [12, 13], fingerprints: 3 },
        })
      )
    ).toBe('merged #12, #13 into this group (3 fingerprints)')
    expect(
      describeActivity(
        entry({ kind: 'split', details: { group_ids: [20], fingerprints: 1 } })
      )
    ).toBe('split fingerprints between this group and #20 (1 fingerprint)')
  })

  it('describes unassigning and falls back for a deleted actor', () => {
    expect(describeActivity(entry({ kind: 'assigned', details: {} }))).toBe(
      'unassigned the group'
    )
    expect(
      describeActivity(
        entry({
          kind: 'assigned',
          details: {
            assignee: { type: 'team', id: 4 },
            assignee_name: 'Payments',
          },
        })
      )
    ).toBe('assigned the group to Payments (team)')
    expect(activityActor(entry({ actor_name: null }))).toBe('Someone')
  })
})

describe('commentPreview', () => {
  it('names the author and the opening words of the comment', () => {
    expect(commentPreview('Looks like a  retry\nloop', 'Ada')).toBe(
      'Ada\'s "Looks like a retry loop"'
    )
  })

  it('cuts a long comment at 40 characters', () => {
    expect(commentPreview('x'.repeat(60), null)).toBe(`"${'x'.repeat(40)}…"`)
  })

  it('falls back to "comment" when the body is empty', () => {
    expect(commentPreview('  ', 'Ada')).toBe("Ada's comment")
    expect(commentPreview(null, null)).toBe('comment')
  })
})
