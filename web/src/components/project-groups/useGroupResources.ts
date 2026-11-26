// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { queryOptions, useQueries } from '@tanstack/react-query'
import { getProjectsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { getProjects } from '@/api/client/sdk.gen'
import type { ProjectResponse } from '@/api/client'
import {
  fanoutMembers,
  groupBackupQueries,
  groupDatabaseQueries,
  groupDeploymentQueries,
  groupDomainQueries,
  mergeGroupBackups,
  mergeGroupDatabases,
  mergeGroupDeployments,
  mergeGroupDomains,
  type GroupDatabase,
  type GroupMember,
} from '@/lib/group-scoped-lists'

/** Members per request; the API caps a page at 100. */
const MEMBER_PAGE_SIZE = 100
const MAX_MEMBER_PAGES = 20

/** Every member of a group as full services, across all pages. */
async function fetchGroupMembers(groupId: number, signal: AbortSignal) {
  const members: ProjectResponse[] = []
  for (let page = 1; page <= MAX_MEMBER_PAGES; page++) {
    const { data } = await getProjects({
      query: { project_group_id: groupId, page, per_page: MEMBER_PAGE_SIZE },
      signal,
      throwOnError: true,
    })
    members.push(...data.projects)
    if (
      data.projects.length < MEMBER_PAGE_SIZE ||
      members.length >= data.total
    ) {
      break
    }
  }
  return members
}

/**
 * The project's services in full (the project itself carries only a
 * summary of each), shared by every page that needs more than the summary.
 */
export function groupServicesQuery(groupId: number) {
  return queryOptions({
    queryKey: getProjectsQueryKey({
      query: { project_group_id: groupId, per_page: MEMBER_PAGE_SIZE },
    }),
    queryFn: ({ signal }) => fetchGroupMembers(groupId, signal),
  })
}

/** State shared by every project-scoped list. */
export interface ScopedListState {
  /** Any request is still loading for the first time. */
  loading: boolean
  /** How many of the per-service (or per-database) requests failed. */
  failed: number
  /** The project has more members than one list requests. */
  truncated: boolean
  /** Re-requests the failed requests; what loaded stays shown. */
  refetch: () => void
}

export function summarize(
  results: readonly {
    isLoading: boolean
    isError: boolean
    refetch: () => unknown
  }[],
  truncated: boolean
): ScopedListState {
  return {
    loading: results.some((r) => r.isLoading),
    failed: results.filter((r) => r.isError).length,
    truncated,
    refetch: () => {
      for (const result of results) if (result.isError) void result.refetch()
    },
  }
}

/** Databases linked to any of the project's services. */
export function useGroupDatabases(members: readonly GroupMember[]) {
  const requested = fanoutMembers(members)
  const results = useQueries({
    queries: groupDatabaseQueries(requested.requested).map((query) => ({
      ...query,
      retry: false,
    })),
  })
  const databases = mergeGroupDatabases(
    requested.requested,
    results.map((r) => r.data)
  )
  return { databases, ...summarize(results, requested.truncated) }
}

/** Custom domains of the project's services. */
export function useGroupDomains(members: readonly GroupMember[]) {
  const requested = fanoutMembers(members)
  const results = useQueries({
    queries: groupDomainQueries(requested.requested).map((query) => ({
      ...query,
      retry: false,
    })),
  })
  const domains = mergeGroupDomains(
    requested.requested,
    results.map((r) => r.data?.domains)
  )
  return { domains, ...summarize(results, requested.truncated) }
}

/** The project's services' recent deployments, newest first. */
export function useGroupDeployments(members: readonly GroupMember[]) {
  const requested = fanoutMembers(members)
  const results = useQueries({
    queries: groupDeploymentQueries(requested.requested).map((query) => ({
      ...query,
      retry: false,
    })),
  })
  const deployments = mergeGroupDeployments(
    requested.requested,
    results.map((r) => r.data?.deployments)
  )
  return { deployments, ...summarize(results, requested.truncated) }
}

/** Recent backups of the given databases (a project's), newest first. */
export function useGroupBackups(databases: readonly GroupDatabase[]) {
  const requested = fanoutMembers(databases)
  const results = useQueries({
    queries: groupBackupQueries(requested.requested.map((db) => db.id)).map(
      (query) => ({ ...query, retry: false })
    ),
  })
  const backups = mergeGroupBackups(
    requested.requested,
    results.map((r) => r.data?.backups)
  )
  return { backups, ...summarize(results, requested.truncated) }
}
