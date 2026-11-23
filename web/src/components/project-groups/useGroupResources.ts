// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQueries } from '@tanstack/react-query'
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

/** State shared by every project-scoped list. */
export interface ScopedListState {
  /** Any request is still loading for the first time. */
  loading: boolean
  /** How many of the per-service (or per-database) requests failed. */
  failed: number
  /** The project has more members than one list requests. */
  truncated: boolean
  refetch: () => void
}

function summarize(
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
      for (const result of results) void result.refetch()
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
