// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The queries behind the internal access lists (ADR 101, ADR 105): each
 * grants lookup asks for every grant of a kind on one project in a single
 * request, and says whether its answer can decide the rows.
 */
import {
  listInternalAccessGrantsOptions,
  listInternalAccessSandboxGrantsOptions,
  listSandboxesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { InternalAccessSandboxGrantListResponse } from '@/api/client/types.gen'
import {
  GRANT_LOOKUP_PAGE_SIZE,
  SANDBOX_LOOKUP_LIMIT,
  grantsLookupComplete,
} from './internal-access'

/** Where a grants lookup stands for the Allow buttons. */
export type GrantsLookup =
  | { state: 'ready' }
  | { state: 'pending' }
  | { state: 'failed'; error: unknown; retry: () => void; retrying: boolean }
  /** More grants than one request returns: no row can be decided. */
  | { state: 'incomplete' }

export function projectGrantsOptions(
  projectId: number,
  environmentId?: number
) {
  return listInternalAccessGrantsOptions({
    path: { project_id: projectId },
    query: {
      environment_id: environmentId,
      page: 1,
      page_size: GRANT_LOOKUP_PAGE_SIZE,
    },
  })
}

/** Every listing of this project's project grants, whatever its filter. */
export function isProjectGrantsQuery(projectId: number) {
  return (query: { queryKey: readonly unknown[] }) => {
    const key = query.queryKey[0] as
      { _id?: string; path?: { project_id?: number } } | undefined
    return (
      key?._id === 'listInternalAccessGrants' &&
      key.path?.project_id === projectId
    )
  }
}

export function sandboxGrantsOptions(
  projectId: number,
  environmentId?: number
) {
  return listInternalAccessSandboxGrantsOptions({
    path: { project_id: projectId },
    query: {
      environment_id: environmentId,
      page: 1,
      page_size: GRANT_LOOKUP_PAGE_SIZE,
    },
  })
}

/** The user's sandboxes, as many as one request returns. */
export function userSandboxesOptions() {
  return listSandboxesOptions({
    query: { page: 1, page_size: SANDBOX_LOOKUP_LIMIT },
  })
}

/**
 * Every listing a sandbox grant on `projectId` changes: the project's sandbox
 * grants, and any sandbox's internal addresses.
 */
export function isSandboxGrantsQuery(projectId: number) {
  return (query: { queryKey: readonly unknown[] }) => {
    const key = query.queryKey[0] as
      { _id?: string; path?: { project_id?: number } } | undefined
    return (
      (key?._id === 'listInternalAccessSandboxGrants' &&
        key.path?.project_id === projectId) ||
      key?._id === 'listSandboxInternalAddresses'
    )
  }
}

/** Where one grants lookup stands, from its query. */
export function lookupOf(query: {
  data?: { items: unknown[]; total: number }
  isError: boolean
  error: unknown
  isFetching: boolean
  refetch: () => unknown
}): GrantsLookup {
  if (query.isError) {
    return {
      state: 'failed',
      error: query.error,
      retry: () => void query.refetch(),
      retrying: query.isFetching,
    }
  }
  if (query.data === undefined) return { state: 'pending' }
  return grantsLookupComplete(query.data)
    ? { state: 'ready' }
    : { state: 'incomplete' }
}

/** Where the lookups for the projects on screen stand, together. */
export function combinedLookup(
  lookups: {
    data?: InternalAccessSandboxGrantListResponse
    isError: boolean
    error: unknown
    isFetching: boolean
    refetch: () => unknown
  }[]
): GrantsLookup {
  const failed = lookups.filter((lookup) => lookup.isError)
  if (failed.length > 0) {
    return {
      state: 'failed',
      error: failed[0].error,
      retry: () => failed.forEach((lookup) => void lookup.refetch()),
      retrying: failed.some((lookup) => lookup.isFetching),
    }
  }
  if (lookups.some((lookup) => lookup.data === undefined)) {
    return { state: 'pending' }
  }
  return lookups.every((lookup) => grantsLookupComplete(lookup.data))
    ? { state: 'ready' }
    : { state: 'incomplete' }
}
