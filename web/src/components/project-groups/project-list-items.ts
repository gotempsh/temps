// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import { projectListPage } from '@/lib/project-list-page'
import { orderProjectList } from '@/lib/project-order'

export type ProjectListItem =
  | { kind: 'group'; key: string; group: ProjectGroupResponse }
  | { kind: 'service'; key: string; project: ProjectResponse }

const toItem = {
  group: (group: ProjectGroupResponse): ProjectListItem => ({
    kind: 'group',
    key: `group-${group.id}`,
    group,
  }),
  service: (project: ProjectResponse): ProjectListItem => ({
    kind: 'service',
    key: `service-${project.id}`,
    project,
  }),
}

/**
 * Projects (groups) and ungrouped services as one list, in the shared
 * project order (`orderProjectList`): most recently deployed first.
 */
export function projectListItems(
  groups: ProjectGroupResponse[],
  standalone: ProjectResponse[]
): ProjectListItem[] {
  return orderProjectList(groups, standalone, toItem)
}

/**
 * One page of that list, `pageSize` items counting projects and services
 * alike (`projectListPage`). `standalone` are the services `serviceWindow`
 * asked for, the first being service number `offset`.
 */
export function projectListItemsPage(
  groups: readonly ProjectGroupResponse[],
  standalone: readonly ProjectResponse[],
  window: { offset: number; page: number; pageSize: number }
): ProjectListItem[] {
  return projectListPage(groups, standalone, window, toItem)
}
