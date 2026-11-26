// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ExternalServiceInfo, ProjectServiceInfo } from '@/api/client'

type Typed = { service: Pick<ExternalServiceInfo, 'service_type'> }

/**
 * Databases on this server the service could link, matching `search` by
 * name or type. A service links one database of each type (the server
 * refuses a second Postgres), so a type it already uses is not offered,
 * nor, with it, the databases already linked.
 */
export function linkableDatabases(
  all: readonly ExternalServiceInfo[],
  linked: readonly Pick<ProjectServiceInfo, 'service'>[],
  search: string
): ExternalServiceInfo[] {
  const term = search.trim().toLowerCase()
  const used = new Set(linked.map((link) => link.service.service_type))
  return all.filter(
    (service) =>
      !used.has(service.service_type) &&
      `${service.name} ${service.service_type}`.toLowerCase().includes(term)
  )
}

/**
 * The apps `database` could be linked to: those using no database of its
 * type yet, this one included. `databases` are the ones the apps use.
 */
export function appsThatCanLink<App extends { id: number }>(
  database: Typed,
  apps: readonly App[],
  databases: readonly (Typed & { usedBy: readonly { id: number }[] })[]
): App[] {
  const type = database.service.service_type
  return apps.filter(
    (app) =>
      !databases.some(
        (other) =>
          other.service.service_type === type &&
          other.usedBy.some((user) => user.id === app.id)
      )
  )
}
