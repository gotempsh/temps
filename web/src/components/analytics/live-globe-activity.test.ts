// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ActivityEvent } from '@/api/client'
import { describe, expect, test } from 'bun:test'
import {
  activityFeedReducer,
  initialActivityFeedState,
} from './live-globe-activity'

function event(id: number): ActivityEvent {
  return { id } as ActivityEvent
}

describe('activityFeedReducer', () => {
  const scopedInitial = activityFeedReducer(initialActivityFeedState, {
    type: 'activate',
    scope: '1:all',
  })

  test('deduplicates events, advances the cursor, and retains newest 100', () => {
    const first = activityFeedReducer(scopedInitial, {
      type: 'merge',
      scope: '1:all',
      events: Array.from({ length: 75 }, (_, index) => event(index + 1)),
    })
    const next = activityFeedReducer(first, {
      type: 'merge',
      scope: '1:all',
      events: Array.from({ length: 76 }, (_, index) => event(index + 50)),
    })

    expect(next.events).toHaveLength(100)
    expect(next.events[0]?.id).toBe(125)
    expect(next.events[next.events.length - 1]?.id).toBe(26)
    expect(next.sinceId).toBe(125)
    expect(next.newEventIds).toEqual(
      new Set(Array.from({ length: 50 }, (_, index) => index + 76))
    )
  })

  test('clears highlights without discarding accumulated events or cursor', () => {
    const merged = activityFeedReducer(scopedInitial, {
      type: 'merge',
      scope: '1:all',
      events: [event(9)],
    })
    const cleared = activityFeedReducer(merged, {
      type: 'clear-highlights',
      scope: '1:all',
    })

    expect(cleared.events).toEqual(merged.events)
    expect(cleared.sinceId).toBe(9)
    expect(cleared.newEventIds.size).toBe(0)
  })

  test('resets accumulated activity before merging a different scope', () => {
    const merged = activityFeedReducer(scopedInitial, {
      type: 'merge',
      scope: '1:all',
      events: [event(9)],
    })

    const activated = activityFeedReducer(merged, {
      type: 'activate',
      scope: '2:7',
    })
    const nextScope = activityFeedReducer(activated, {
      type: 'merge',
      scope: '2:7',
      events: [event(2)],
    })

    expect(nextScope.scope).toBe('2:7')
    expect(nextScope.events).toEqual([event(2)])
    expect(nextScope.sinceId).toBe(2)

    expect(
      activityFeedReducer(nextScope, {
        type: 'merge',
        scope: '1:all',
        events: [event(99)],
      })
    ).toBe(nextScope)
  })
})
