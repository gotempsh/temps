// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The sidebar as a tree of what the user owns: projects with their
 * services, databases, domains and servers. Data (logs, errors, traces) is
 * not an entry of the tree; it is shown on the page of the thing it belongs
 * to. This module holds the tree's rules, apart from how it is drawn.
 */

import { resolveConsoleScope } from '@/lib/console-scope'
import { resolveServerLocation } from '@/lib/server-navigation'

export type TreeBranchId = 'projects' | 'databases' | 'domains' | 'servers'

/** The server areas each branch stands for, by area id. */
const BRANCH_OF_AREA: Record<string, TreeBranchId> = {
  projects: 'projects',
  logs: 'projects',
  errors: 'projects',
  traces: 'projects',
  analytics: 'projects',
  alerts: 'projects',
  databases: 'databases',
  domains: 'domains',
  servers: 'servers',
}

/** Record pages of a branch: `/storage/12`, `/domains/4`, a node. */
const RECORD_PATHS: [TreeBranchId, RegExp][] = [
  ['databases', /^\/storage\/(\d+)(?:\/|$)/],
  ['domains', /^\/domains\/(\d+)(?:\/|$)/],
  ['servers', /^\/settings\/nodes\/(\d+)(?:\/|$)/],
]

export interface TreeLocation {
  /** The branch the page belongs to; absent in Settings and elsewhere. */
  branch?: TreeBranchId
  /** The project (an API project group) shown, by slug. */
  groupSlug?: string
  /** The service (an API project) shown, by slug. */
  serviceSlug?: string
  /** The database, domain or server shown, by id. */
  recordId?: number
}

/** Where a URL sits in the tree, so its ancestors open on their own. */
export function locateInTree(pathname: string): TreeLocation {
  const scope = resolveConsoleScope(pathname)
  if (scope.kind === 'project')
    return { branch: 'projects', groupSlug: scope.groupSlug }
  if (scope.kind === 'service')
    return { branch: 'projects', serviceSlug: scope.serviceSlug }
  for (const [branch, pattern] of RECORD_PATHS) {
    const match = pathname.match(pattern)
    if (match) return { branch, recordId: Number(match[1]) }
  }
  const area = resolveServerLocation(pathname).area
  return { branch: area ? BRANCH_OF_AREA[area.id] : undefined }
}

/** What a row says about its entity, in a colour and in words. */
export interface TreeState {
  tone: 'ok' | 'warn' | 'bad' | 'idle'
  label: string
}

const state = (tone: TreeState['tone'], label: string): TreeState => ({
  tone,
  label,
})

/**
 * A service's state from its uptime monitors. A service nothing checks has
 * none: the tree does not claim it is healthy.
 */
export function serviceState(
  status: string | undefined
): TreeState | undefined {
  switch (status) {
    case 'operational':
      return state('ok', 'Healthy')
    case 'degraded':
      return state('warn', 'Degraded')
    case 'down':
      return state('bad', 'Down')
    case 'not_deployed':
      return state('idle', 'Not deployed')
    default:
      return undefined
  }
}

export function databaseState(status: string): TreeState | undefined {
  switch (status) {
    case 'running':
    case 'active':
    case 'ready':
      return state('ok', 'Running')
    case 'error':
    case 'failed':
      return state('bad', 'Failed')
    case 'pending':
    case 'initializing':
    case 'creating':
    case 'starting':
      return state('warn', 'Starting')
    case 'stopped':
      return state('idle', 'Stopped')
    default:
      return undefined
  }
}

export function domainState(status: string): TreeState | undefined {
  switch (status) {
    case 'active':
      return state('ok', 'Active')
    case 'active_renewal_failed':
      return state('warn', 'Renewal failed')
    case 'failed':
      return state('bad', 'Failed')
    case 'pending':
    case 'pending_dns':
      return state('warn', 'Pending')
    default:
      return undefined
  }
}

export function serverState(status: string): TreeState | undefined {
  switch (status) {
    case 'active':
      return state('ok', 'Active')
    case 'draining':
      return state('warn', 'Draining')
    case 'drained':
      return state('warn', 'Drained')
    case 'offline':
      return state('bad', 'Offline')
    default:
      return undefined
  }
}

const SEVERITY: Record<TreeState['tone'], number> = {
  idle: 0,
  ok: 1,
  warn: 2,
  bad: 3,
}

/**
 * What a folded project says for its services: the worst of what is known,
 * counted when it is bad news ("2 down"), so trouble shows without opening
 * it. Unknown services do not count either way.
 */
export function worstState(
  states: readonly (TreeState | undefined)[]
): TreeState | undefined {
  let worst: TreeState | undefined
  for (const candidate of states) {
    if (
      candidate &&
      (!worst || SEVERITY[candidate.tone] > SEVERITY[worst.tone])
    )
      worst = candidate
  }
  if (!worst || worst.tone === 'ok' || worst.tone === 'idle') return worst
  const same = states.filter((candidate) => candidate?.label === worst.label)
  return same.length > 1
    ? state(worst.tone, `${same.length} ${worst.label.toLowerCase()}`)
    : worst
}

export interface ServiceNode {
  id: number
  name: string
  slug: string
  source_type?: string | null
}

/** A top-level row under Projects. */
export type ProjectNode =
  | {
      kind: 'project'
      key: string
      id: number
      name: string
      slug: string
      services: ServiceNode[]
    }
  /** A service that belongs to no project is listed beside them. */
  | { kind: 'service'; key: string; service: ServiceNode }

/**
 * The rows under Projects: every project with its services, then the
 * services that are in none.
 */
export function projectNodes(
  groups: readonly {
    id: number
    name: string
    slug: string
    projects: readonly ServiceNode[]
  }[],
  services: readonly ServiceNode[]
): ProjectNode[] {
  const grouped = new Set<number>()
  for (const group of groups)
    for (const member of group.projects) grouped.add(member.id)
  return [
    ...groups.map((group): ProjectNode => ({
      kind: 'project',
      key: `project:${group.id}`,
      id: group.id,
      name: group.name,
      slug: group.slug,
      services: [...group.projects],
    })),
    ...services
      .filter((service) => !grouped.has(service.id))
      .map((service): ProjectNode => ({
        kind: 'service',
        key: `service:${service.id}`,
        service,
      })),
  ]
}

/**
 * The first `limit` rows of a list, and how many are left out. The row being
 * shown is always among them: it takes the last place when it falls past
 * the limit, so the tree never loses where the user is.
 */
export function capRows<T>(
  rows: readonly T[],
  limit: number,
  isCurrent: (row: T) => boolean
): { shown: T[]; more: number } {
  if (rows.length <= limit) return { shown: [...rows], more: 0 }
  const shown = rows.slice(0, limit)
  const current = rows.findIndex(isCurrent)
  if (current >= limit) shown[limit - 1] = rows[current]
  return { shown, more: rows.length - limit }
}
