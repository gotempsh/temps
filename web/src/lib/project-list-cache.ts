// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QueryClient } from '@tanstack/react-query'
import {
  getProjectsQueryKey,
  listProjectGroupsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'

/**
 * Refresh every cached project list after a project is created.
 *
 * Two key shapes hold project lists and neither prefix reaches the other:
 * - generated SDK keys, `[{ _id: 'getProjects', ... }]`, used by the pages that
 *   call `getProjects*Options()`;
 * - `ProjectsProvider`'s hand-written, user-scoped key
 *   `['getProjects', { page, per_page }, userId]`, which is reached by the
 *   `['getProjects']` string prefix.
 */
export async function invalidateProjectLists(
  queryClient: QueryClient
): Promise<void> {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: getProjectsQueryKey() }),
    queryClient.invalidateQueries({ queryKey: ['getProjects'] }),
  ])
}

/**
 * Refresh every list a deleted service appeared in: the project lists (the
 * Projects page, the sidebar, pickers) and the projects (groups) that list it
 * as a member. The sidebar stays mounted across the navigation that follows
 * a delete and holds its lists fresh for a minute, so without this it keeps
 * showing the deleted service.
 *
 * The deleted service's own records are left alone: the page that deleted it
 * is still mounted, and refetching them would only answer 404.
 */
export async function invalidateListsAfterServiceDelete(
  queryClient: QueryClient
): Promise<void> {
  await Promise.all([
    invalidateProjectLists(queryClient),
    queryClient.invalidateQueries({ queryKey: listProjectGroupsQueryKey() }),
    // A project's page lists its services; the generated key helpers need
    // a path, so every project's record is matched by its `_id` alone.
    queryClient.invalidateQueries({ queryKey: [{ _id: 'getProjectGroup' }] }),
    queryClient.invalidateQueries({
      queryKey: [{ _id: 'getProjectGroupBySlug' }],
    }),
  ])
}
