// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'
import { orderProjectList } from '@/lib/project-order'

export type ProjectListItem =
  | { kind: 'group'; key: string; group: ProjectGroupResponse }
  | { kind: 'service'; key: string; project: ProjectResponse }

/**
 * Projects (groups) and ungrouped services as one list, in the shared
 * project order (`orderProjectList`): most recently deployed first.
 */
export function projectListItems(
  groups: ProjectGroupResponse[],
  standalone: ProjectResponse[]
): ProjectListItem[] {
  return orderProjectList<
    ProjectGroupResponse,
    ProjectResponse,
    ProjectListItem
  >(groups, standalone, {
    group: (group) => ({ kind: 'group', key: `group-${group.id}`, group }),
    service: (project) => ({
      kind: 'service',
      key: `service-${project.id}`,
      project,
    }),
  })
}
