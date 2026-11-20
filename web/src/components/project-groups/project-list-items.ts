// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectGroupResponse, ProjectResponse } from '@/api/client'

export type ProjectListItem =
  | { kind: 'group'; key: string; group: ProjectGroupResponse }
  | { kind: 'service'; key: string; project: ProjectResponse }

/** Epoch ms; services send epoch ms, project members an ISO string. */
function deployedAt(value?: string | number | null): number {
  if (value === null || value === undefined) return -Infinity
  const time = typeof value === 'number' ? value : Date.parse(value)
  return Number.isNaN(time) ? -Infinity : time
}

/**
 * Projects (groups) and ungrouped services as one list, ordered the way the
 * services endpoint orders services: most recently deployed first, never
 * deployed last. A project counts as deployed when any of its services was.
 * Ties keep projects ahead of services, then the incoming order.
 */
export function projectListItems(
  groups: ProjectGroupResponse[],
  standalone: ProjectResponse[]
): ProjectListItem[] {
  const items = [
    ...groups.map((group) => ({
      item: { kind: 'group', key: `group-${group.id}`, group } as ProjectListItem,
      at: Math.max(-Infinity, ...group.projects.map((p) => deployedAt(p.last_deployment))),
    })),
    ...standalone.map((project) => ({
      item: { kind: 'service', key: `service-${project.id}`, project } as ProjectListItem,
      at: deployedAt(project.last_deployment),
    })),
  ]
  // Array.prototype.sort is stable, so equal timestamps keep the order above.
  return items.sort((a, b) => (a.at === b.at ? 0 : b.at - a.at)).map((x) => x.item)
}
