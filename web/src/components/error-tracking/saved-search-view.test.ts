// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  filtersFromView,
  viewFromFilters,
  viewMatches,
  type GroupListView,
} from './saved-search-view'

const view: GroupListView = {
  status: 'unresolved',
  assignee: 'me',
  sort: 'events_in_range:desc',
  environmentId: 3,
  search: '  timeout ',
}

describe('saved search views', () => {
  it('stores only what the view filters on', () => {
    expect(filtersFromView(view)).toEqual({
      status: 'unresolved',
      assignee: 'me',
      environment_id: 3,
      search: 'timeout',
      sort_by: 'events_in_range',
      sort_order: 'desc',
    })
    expect(
      filtersFromView({
        status: 'all',
        assignee: 'any',
        sort: 'last_seen:desc',
        search: '',
      })
    ).toEqual({
      status: null,
      assignee: null,
      environment_id: null,
      search: null,
      sort_by: 'last_seen',
      sort_order: 'desc',
    })
  })

  it('opens a saved search as the view it was saved from', () => {
    const reopened = viewFromFilters(filtersFromView(view))
    expect(reopened).toEqual({ ...view, search: 'timeout' })
    expect(viewMatches(view, filtersFromView(view))).toBe(true)
  })

  it('falls back to the defaults for what a search leaves out', () => {
    expect(viewFromFilters({})).toEqual({
      status: 'all',
      assignee: 'any',
      sort: 'last_seen:desc',
      environmentId: undefined,
      search: '',
    })
    expect(viewFromFilters({ assignee: 'team:4', sort_order: 'asc' })).toEqual({
      status: 'all',
      assignee: 'team:4',
      sort: 'last_seen:asc',
      environmentId: undefined,
      search: '',
    })
  })
})
