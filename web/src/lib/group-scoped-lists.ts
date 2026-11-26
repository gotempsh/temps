// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Lists scoped to one project (an API `project_group`), built only from
 * endpoints the server already scopes to a single service or database. The
 * console never loads a server-wide list and filters it in the browser.
 *
 * The API has no `project_group_id` filter on these lists yet, so a project's
 * list is the union of its services' lists: one request per service (or per
 * database for backups). Projects hold a handful of services, and the fan-out
 * is capped at `GROUP_FANOUT_LIMIT`; past that the page says the list is
 * partial. The backend follow-ups that replace the fan-out with one request
 * each are listed in docs/adr (project-focused console).
 */

import type {
  CustomDomainResponse,
  DeploymentResponse,
  ExternalServiceInfo,
  ProjectServiceInfo,
  ServiceBackupEntryResponse,
  ServiceTypeRoute,
} from '@/api/client/types.gen'
import {
  getProjectDeploymentsOptions,
  listCustomDomainsForProjectOptions,
  listExternalServiceBackupsOptions,
  listProjectServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'

/** Most services (or databases) a scoped list requests at once. */
export const GROUP_FANOUT_LIMIT = 25

/** Recent deployments requested per service for the project's list. */
export const DEPLOYMENTS_PER_SERVICE = 10

/** Recent backups requested per database for the project's list. */
export const BACKUPS_PER_DATABASE = 10

export interface GroupMember {
  id: number
  name: string
  slug: string
}

/** The members a scoped list requests, and whether some were left out. */
export function fanoutMembers<T>(members: readonly T[]): {
  requested: T[]
  truncated: boolean
} {
  return {
    requested: members.slice(0, GROUP_FANOUT_LIMIT),
    truncated: members.length > GROUP_FANOUT_LIMIT,
  }
}

export function groupDatabaseQueries(members: readonly GroupMember[]) {
  return fanoutMembers(members).requested.map((member) =>
    listProjectServicesOptions({ path: { project_id: member.id } })
  )
}

export function groupDomainQueries(members: readonly GroupMember[]) {
  return fanoutMembers(members).requested.map((member) =>
    listCustomDomainsForProjectOptions({ path: { project_id: member.id } })
  )
}

export function groupDeploymentQueries(members: readonly GroupMember[]) {
  return fanoutMembers(members).requested.map((member) =>
    getProjectDeploymentsOptions({
      path: { id: member.id },
      query: { page: 1, per_page: DEPLOYMENTS_PER_SERVICE },
    })
  )
}

export function groupBackupQueries(databaseIds: readonly number[]) {
  return fanoutMembers(databaseIds).requested.map((service_id) =>
    listExternalServiceBackupsOptions({
      path: { service_id },
      query: { page: 1, page_size: BACKUPS_PER_DATABASE },
    })
  )
}

export interface GroupDatabase {
  id: number
  name: string
  type: ServiceTypeRoute
  status: string
  /** The database as the server reports it: version, topology, metrics. */
  service: ExternalServiceInfo
  /** The project's services linked to it. */
  usedBy: GroupMember[]
  /** How each of those services is linked (database per environment...). */
  links: { member: GroupMember; link: ProjectServiceInfo }[]
}

/**
 * The databases linked to at least one of the project's services, each once,
 * with the services that use it. `results[i]` answers `members[i]`.
 */
export function mergeGroupDatabases(
  members: readonly GroupMember[],
  results: readonly (ProjectServiceInfo[] | undefined)[]
): GroupDatabase[] {
  const byId = new Map<number, GroupDatabase>()
  results.forEach((links, index) => {
    const member = members[index]
    for (const link of links ?? []) {
      const entry = byId.get(link.service.id) ?? {
        id: link.service.id,
        name: link.service.name,
        type: link.service.service_type,
        status: link.service.status,
        service: link.service,
        usedBy: [],
        links: [],
      }
      if (member && !entry.usedBy.some((m) => m.id === member.id)) {
        entry.usedBy.push(member)
        entry.links.push({ member, link })
      }
      byId.set(link.service.id, entry)
    }
  })
  return [...byId.values()].sort((a, b) => a.name.localeCompare(b.name))
}

export interface GroupDomain extends CustomDomainResponse {
  service: GroupMember
}

/** Every custom domain of the project's services, by hostname. */
export function mergeGroupDomains(
  members: readonly GroupMember[],
  results: readonly (CustomDomainResponse[] | undefined)[]
): GroupDomain[] {
  return results
    .flatMap((domains, index) => {
      const service = members[index]
      return service
        ? (domains ?? []).map((domain) => ({ ...domain, service }))
        : []
    })
    .sort((a, b) => a.domain.localeCompare(b.domain))
}

export interface GroupDeployment extends DeploymentResponse {
  service: GroupMember
}

/** The services' recent deployments, newest first. */
export function mergeGroupDeployments(
  members: readonly GroupMember[],
  results: readonly (DeploymentResponse[] | undefined)[]
): GroupDeployment[] {
  return results
    .flatMap((deployments, index) => {
      const service = members[index]
      return service
        ? (deployments ?? []).map((deployment) => ({ ...deployment, service }))
        : []
    })
    .sort(
      (a, b) =>
        Date.parse(b.created_at) - Date.parse(a.created_at) || b.id - a.id
    )
}

export interface GroupBackup extends ServiceBackupEntryResponse {
  database: Pick<GroupDatabase, 'id' | 'name' | 'type'>
}

/** The databases' recent backups, newest first. */
export function mergeGroupBackups(
  databases: readonly Pick<GroupDatabase, 'id' | 'name' | 'type'>[],
  results: readonly (ServiceBackupEntryResponse[] | undefined)[]
): GroupBackup[] {
  return results
    .flatMap((backups, index) => {
      const database = databases[index]
      return database
        ? (backups ?? []).map((backup) => ({ ...backup, database }))
        : []
    })
    .sort(
      (a, b) =>
        Date.parse(b.started_at) - Date.parse(a.started_at) || b.id - a.id
    )
}
