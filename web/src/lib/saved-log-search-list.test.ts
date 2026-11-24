// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { SavedSearchResponse } from '@/api/client/types.gen'
import { alertOnSearchHref, orderSavedSearches } from './saved-log-search'

const search = (
  id: number,
  name: string,
  pinned: boolean
): SavedSearchResponse =>
  ({ id, name, pinned }) as unknown as SavedSearchResponse

describe('orderSavedSearches', () => {
  test('pinned first, then by name', () => {
    const ordered = orderSavedSearches([
      search(1, 'zeta', false),
      search(2, 'beta', true),
      search(3, 'alpha', false),
    ])
    expect(ordered.map((s) => s.id)).toEqual([2, 3, 1])
  })
})

describe('alertOnSearchHref', () => {
  test('opens a new alert rule with the search chosen', () => {
    expect(alertOnSearchHref('my app', 4)).toBe(
      '/projects/my%20app/metrics/alerts/new?saved_search_id=4'
    )
  })
})
