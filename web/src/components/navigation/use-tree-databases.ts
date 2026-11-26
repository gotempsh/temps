// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQueries } from '@tanstack/react-query'
import { listProjectServicesOptions } from '@/api/client/@tanstack/react-query.gen'
import { databaseRequests, type ServiceNode } from '@/lib/entity-tree'
import {
  GROUP_FANOUT_LIMIT,
  mergeGroupDatabases,
  type GroupDatabase,
} from '@/lib/group-scoped-lists'

/** The databases an open project's services use, in whichever state. */
export interface ProjectDatabases {
  databases: GroupDatabase[]
  /** Some of its services have not answered yet. */
  loading: boolean
  /** Some of its services could not be asked. */
  failed: boolean
  /** Some of its services were not asked: its page lists every database. */
  truncated: boolean
  /** Asks the services that failed again. */
  refetch: () => void
}

/**
 * The databases the services of each open project use, by project key. A
 * folded project asks nothing, and all open projects together ask for at
 * most as many services as a project's own pages do. The answers are the
 * ones the project's Databases page reads, so opening it costs nothing.
 */
export function useTreeDatabases(
  projects: readonly { key: string; services: readonly ServiceNode[] }[]
): Map<string, ProjectDatabases> {
  const { requests, truncated } = databaseRequests(projects, GROUP_FANOUT_LIMIT)
  const results = useQueries({
    queries: requests.map((request) => ({
      ...listProjectServicesOptions({
        path: { project_id: request.service.id },
      }),
      staleTime: 60_000,
      retry: false,
    })),
  })
  const byProject = new Map<string, ProjectDatabases>()
  for (const project of projects) {
    const indices = requests
      .map((request, index) => (request.key === project.key ? index : -1))
      .filter((index) => index >= 0)
    const answered = indices.map((index) => results[index])
    byProject.set(project.key, {
      databases: mergeGroupDatabases(
        indices.map((index) => requests[index].service),
        answered.map((result) => result.data)
      ),
      loading: answered.some((result) => result.isLoading),
      failed: answered.some((result) => result.isError),
      truncated: truncated.has(project.key),
      refetch: () => {
        for (const result of answered) if (result.isError) void result.refetch()
      },
    })
  }
  return byProject
}
