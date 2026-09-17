// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ActivityEvent } from '@/api/client'

export interface ActivityFeedState {
  scope: string | null
  events: ActivityEvent[]
  newEventIds: Set<number>
  sinceId: number | null
}

export type ActivityFeedAction =
  | { type: 'activate'; scope: string }
  | { type: 'merge'; scope: string; events: ActivityEvent[] }
  | { type: 'clear-highlights'; scope: string }

export const initialActivityFeedState: ActivityFeedState = {
  scope: null,
  events: [],
  newEventIds: new Set(),
  sinceId: null,
}

export function activityFeedReducer(
  state: ActivityFeedState,
  action: ActivityFeedAction
): ActivityFeedState {
  if (action.type === 'activate') {
    return state.scope === action.scope
      ? state
      : { ...initialActivityFeedState, scope: action.scope }
  }
  if (action.type === 'clear-highlights') {
    return state.scope !== action.scope || state.newEventIds.size === 0
      ? state
      : { ...state, newEventIds: new Set() }
  }

  // An old request may settle after the user changes project/environment.
  // Never let that response replace the newly activated scope.
  if (state.scope !== action.scope) return state
  const scopedState = state
  const existingIds = new Set(scopedState.events.map((event) => event.id))
  const newEvents = action.events.filter((event) => !existingIds.has(event.id))
  if (newEvents.length === 0) return scopedState

  const maxId = Math.max(...action.events.map((event) => event.id))
  return {
    scope: action.scope,
    events: [...newEvents, ...scopedState.events]
      .sort((a, b) => b.id - a.id)
      .slice(0, 100),
    newEventIds: new Set(newEvents.map((event) => event.id)),
    sinceId:
      scopedState.sinceId === null || maxId > scopedState.sinceId
        ? maxId
        : scopedState.sinceId,
  }
}
