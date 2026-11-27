// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** A service as far as ordering cares: when it was last deployed. */
export interface Deployed {
  /** Epoch ms from the services endpoint, an ISO string from project members. */
  last_deployment?: string | number | null
}

/** Epoch ms of a deployment time; never deployed (or unreadable) is -Infinity. */
export function deployedAt(value?: string | number | null): number {
  if (value === null || value === undefined) return -Infinity
  const time = typeof value === 'number' ? value : Date.parse(value)
  return Number.isNaN(time) ? -Infinity : time
}

/** The latest deployment of any of `services`; -Infinity when none was. */
export function latestDeployment(services: readonly Deployed[]): number {
  let latest = -Infinity
  for (const service of services)
    latest = Math.max(latest, deployedAt(service.last_deployment))
  return latest
}

/**
 * Projects (groups) and the services in none as one list, in the order the
 * Projects page and the sidebar both show them: most recently deployed
 * first, never deployed last. A project counts as deployed when any of its
 * services was. Ties keep projects ahead of services, then the incoming
 * order. One helper, so the two surfaces cannot drift apart again.
 */
export function orderProjectList<
  G extends { projects: readonly Deployed[] },
  S extends Deployed,
  R,
>(
  groups: readonly G[],
  services: readonly S[],
  build: { group: (group: G) => R; service: (service: S) => R }
): R[] {
  const entries = [
    ...groups.map((group) => ({
      item: build.group(group),
      at: latestDeployment(group.projects),
    })),
    ...services.map((service) => ({
      item: build.service(service),
      at: deployedAt(service.last_deployment),
    })),
  ]
  // Array.prototype.sort is stable, so equal times keep the order above
  // (and -Infinity - -Infinity is NaN, hence the explicit equality).
  return entries
    .sort((a, b) => (a.at === b.at ? 0 : b.at - a.at))
    .map((entry) => entry.item)
}
