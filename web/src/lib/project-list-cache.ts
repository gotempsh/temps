// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QueryClient } from '@tanstack/react-query'
import { getProjectsQueryKey } from '@/api/client/@tanstack/react-query.gen'

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
