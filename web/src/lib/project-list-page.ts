// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  deployedAt,
  latestDeployment,
  orderProjectList,
  type Deployed,
} from '@/lib/project-order'

/** The most services the API returns in one page. */
export const MAX_SERVICES_PER_REQUEST = 100

/**
 * The services requests one page of the Projects list needs.
 *
 * The list is every project (group) and every service in none, in one order
 * (`orderProjectList`), cut into pages of `pageSize`. Projects are all loaded
 * at once; services come from the server a page at a time, in the same
 * order. Page `page` covers positions `[start, start + pageSize)`, and since
 * between none and all of the `groupCount` projects come before any
 * service, only services `[start - groupCount, start + pageSize)` can land
 * on it. That window is read as `pages` of `perPage` services, the first of
 * them starting at service `offset`: one request, two when it straddles a
 * page boundary (more only past `MAX_SERVICES_PER_REQUEST` projects).
 */
export function serviceWindow(
  page: number,
  pageSize: number,
  groupCount: number
): { perPage: number; pages: number[]; offset: number } {
  const start = (page - 1) * pageSize
  const from = Math.max(0, start - groupCount)
  const to = start + pageSize
  const perPage = Math.min(
    MAX_SERVICES_PER_REQUEST,
    Math.max(pageSize, to - from)
  )
  const first = Math.floor(from / perPage) + 1
  const last = Math.ceil(to / perPage)
  const pages: number[] = []
  for (let p = first; p <= last; p++) pages.push(p)
  return { perPage, pages, offset: (first - 1) * perPage }
}

/**
 * Page `page` of the Projects list: exactly `pageSize` items (fewer only on
 * the last page), projects and services together in the shared order.
 *
 * `services` are the ones `serviceWindow` asked for, in the server's order,
 * the first of them being service number `offset`.
 */
export function projectListPage<
  G extends { projects: readonly Deployed[] },
  S extends Deployed,
  R,
>(
  groups: readonly G[],
  services: readonly S[],
  window: { offset: number; page: number; pageSize: number },
  build: { group: (group: G) => R; service: (service: S) => R }
): R[] {
  const { offset, page, pageSize } = window
  const start = (page - 1) * pageSize
  // Projects in the shared order (stable: equal times keep theirs).
  const ordered = groups
    .map((group, index) => ({
      group,
      index,
      at: latestDeployment(group.projects),
    }))
    .sort((a, b) => (a.at === b.at ? a.index - b.index : b.at - a.at))
  // A service's position in the whole list is its own index plus the
  // projects ahead of it: those deployed at the same time or later.
  const position = (service: S, index: number) => {
    const at = deployedAt(service.last_deployment)
    return offset + index + ordered.filter((g) => g.at >= at).length
  }
  // Services before the window all sit before `start` (see serviceWindow).
  let servicesBefore = offset
  services.forEach((service, index) => {
    if (position(service, index) < start) servicesBefore++
  })
  const groupsBefore = Math.min(
    ordered.length,
    Math.max(0, start - servicesBefore)
  )
  return orderProjectList(
    ordered.slice(groupsBefore).map((entry) => entry.group),
    services.slice(servicesBefore - offset),
    build
  ).slice(0, pageSize)
}
