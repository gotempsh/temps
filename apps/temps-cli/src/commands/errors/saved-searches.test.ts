// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { applySavedSearch, describeFilters, filtersFromFlags, type SavedSearchListFlags } from './saved-searches.js'

describe('applySavedSearch', () => {
  test('fills in what the command line leaves out, and flags win', () => {
    const flags: SavedSearchListFlags & { projectId: string } = { projectId: '4', status: 'resolved' }
    const applied = applySavedSearch(
      flags,
      { status: 'unresolved', assignee: 'me', environment_id: 2, sort_by: 'first_seen', sort_order: 'asc' },
    )
    expect(applied).toEqual({
      projectId: '4',
      status: 'resolved',
      environmentId: '2',
      assignee: 'me',
      search: undefined,
      sortBy: 'first_seen',
      sortOrder: 'asc',
    })
  })
})

describe('filtersFromFlags', () => {
  test('maps flags to stored filters and checks the environment id', () => {
    expect(filtersFromFlags({ status: 'unresolved', environmentId: '7', search: 'timeout' })).toEqual({
      status: 'unresolved',
      environment_id: 7,
      assignee: null,
      search: 'timeout',
      sort_by: null,
      sort_order: null,
    })
    expect(() => filtersFromFlags({ environmentId: 'staging' })).toThrow(/Invalid environment ID/)
  })
})

describe('describeFilters', () => {
  test('sums up a search', () => {
    expect(describeFilters({ status: 'unresolved', assignee: 'team:3', search: 'db' })).toBe(
      'status unresolved, assignee team:3, "db", by last_seen desc',
    )
    expect(describeFilters({})).toBe('any status, by last_seen desc')
  })
})
