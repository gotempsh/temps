// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { SavedSearchFilters } from '@/api/client/types.gen'

export type GroupStatusView = 'unresolved' | 'resolved' | 'ignored' | 'all'

/** The error list's filters as the page holds them. */
export interface GroupListView {
  status: GroupStatusView
  /** `any`, `me`, `none`, `user:<id>` or `team:<id>`. */
  assignee: string
  /** `<sort_by>:<sort_order>`, e.g. `last_seen:desc`. */
  sort: string
  environmentId?: number
  search: string
}

export const DEFAULT_SORT = 'last_seen:desc'
const STATUSES: GroupStatusView[] = ['unresolved', 'resolved', 'ignored']

/** The filters a saved search stores for the current view. */
export function filtersFromView(view: GroupListView): SavedSearchFilters {
  const [sortBy, sortOrder] = view.sort.split(':')
  const search = view.search.trim()
  return {
    status: view.status === 'all' ? null : view.status,
    assignee: view.assignee === 'any' ? null : view.assignee,
    environment_id: view.environmentId ?? null,
    search: search || null,
    sort_by: sortBy || null,
    sort_order: sortOrder || null,
  }
}

/**
 * The view a saved search opens. A filter the search leaves out falls back
 * to the page default, so applying it always gives the same list.
 */
export function viewFromFilters(filters: SavedSearchFilters): GroupListView {
  const status = STATUSES.find((s) => s === filters.status) ?? 'all'
  const sortBy = filters.sort_by || 'last_seen'
  const sortOrder = filters.sort_order === 'asc' ? 'asc' : 'desc'
  return {
    status,
    assignee: filters.assignee || 'any',
    sort: `${sortBy}:${sortOrder}`,
    environmentId: filters.environment_id ?? undefined,
    search: filters.search ?? '',
  }
}

/** Whether the view is exactly what a saved search opens. */
export function viewMatches(
  view: GroupListView,
  filters: SavedSearchFilters
): boolean {
  const saved = viewFromFilters(filters)
  return (
    saved.status === view.status &&
    saved.assignee === view.assignee &&
    saved.sort === view.sort &&
    saved.environmentId === view.environmentId &&
    saved.search === view.search.trim()
  )
}
