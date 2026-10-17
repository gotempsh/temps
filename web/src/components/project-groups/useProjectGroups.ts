// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  getProjectsOptions,
  listProjectGroupsQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { listProjectGroups } from '@/api/client/sdk.gen'
import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import { shouldRetryQuery } from '@/lib/query-retry'

/** Groups per request; the API caps a page at 100. */
export const GROUP_PAGE_SIZE = 100
/**
 * Pages of groups loaded before stopping; anything past it is disclosed
 * through `truncated` rather than dropped silently.
 */
const MAX_GROUP_PAGES = 50
/** Services per page in pickers; the filter searches the rest server-side. */
export const SERVICE_PICKER_PAGE_SIZE = 50

/**
 * A 200 whose body is not a page of groups. Asking again returns the same
 * body, so it is never retried (see `shouldRetryGroupList`).
 */
export class MalformedGroupListError extends Error {
  constructor(page: number) {
    super(
      `Project group list page ${page} did not contain a "groups" array and a numeric "total"`
    )
    this.name = 'MalformedGroupListError'
  }
}

/** Retry transient failures only: a malformed body will not fix itself. */
export function shouldRetryGroupList(
  failureCount: number,
  error: unknown
): boolean {
  if (error instanceof MalformedGroupListError) return false
  return shouldRetryQuery(failureCount, error)
}

async function fetchAllGroups(signal: AbortSignal) {
  const groups: ProjectGroupResponse[] = []
  let total = 0
  for (let page = 1; page <= MAX_GROUP_PAGES; page++) {
    const { data } = await listProjectGroups({
      query: { page, page_size: GROUP_PAGE_SIZE },
      signal,
      throwOnError: true,
    })
    if (!Array.isArray(data?.groups) || typeof data.total !== 'number') {
      throw new MalformedGroupListError(page)
    }
    total = data.total
    groups.push(...data.groups)
    if (data.groups.length < GROUP_PAGE_SIZE || groups.length >= total) break
  }
  return { groups, total }
}

/**
 * Every project (group) the caller can see, across all pages. Groups are
 * few next to services, and grouping, pickers and the command palette need
 * to know each service's group.
 */
export function useProjectGroups() {
  const query = useQuery({
    // Its own key (every page, not one), under the `_id` the group
    // invalidation below matches.
    queryKey: [
      {
        ...listProjectGroupsQueryKey({
          query: { page_size: GROUP_PAGE_SIZE },
        })[0],
        allPages: true,
      },
    ],
    queryFn: ({ signal }) => fetchAllGroups(signal),
    retry: shouldRetryGroupList,
  })
  const groups = useMemo(() => query.data?.groups ?? [], [query.data])
  const truncated = (query.data?.total ?? 0) > groups.length
  const groupByProjectId = useMemo(() => {
    const map = new Map<number, ProjectGroupResponse>()
    for (const group of groups) {
      for (const member of group.projects) map.set(member.id, group)
    }
    return map
  }, [groups])
  return { ...query, groups, groupByProjectId, truncated }
}

/**
 * One page of services for a picker, filtered server-side by name or slug.
 * `enabled` keeps closed dialogs from loading it.
 */
export function useServiceSearch(search: string, enabled: boolean) {
  const query = useQuery({
    ...getProjectsOptions({
      query: {
        page: 1,
        per_page: SERVICE_PICKER_PAGE_SIZE,
        search: search.trim() || undefined,
      },
    }),
    enabled,
    placeholderData: (previous) => previous,
  })
  const services: ProjectResponse[] = useMemo(
    () => query.data?.projects ?? [],
    [query.data]
  )
  const total = query.data?.total ?? 0
  return { ...query, services, total, truncated: total > services.length }
}

/** Refresh every view that shows group membership after a write. */
export function useInvalidateGroups() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({
        predicate: (q) => {
          const key = q.queryKey[0] as { _id?: string } | undefined
          return (
            typeof key === 'object' &&
            key !== null &&
            typeof key._id === 'string' &&
            (key._id.includes('ProjectGroup') ||
              key._id === 'getGroupRepository' ||
              key._id === 'getInheritedEnvVars' ||
              key._id === 'listGroupEnvVars' ||
              key._id === 'listInstantiations' ||
              // Services created or repointed from a project (monorepo
              // import, repository change) show up in these lists.
              key._id === 'getProjects' ||
              key._id === 'getProject' ||
              key._id === 'getProjectBySlug')
          )
        },
      }),
    ])
}
