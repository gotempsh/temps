// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The sidebar as a tree of what the user owns: projects, each with its
 * services and the databases they use, then one Server row for every
 * record of the server. Data (logs, errors, traces) is not an entry of the
 * tree; it is shown on the page of the thing it belongs to. This module
 * holds the tree's rules, apart from how it is drawn.
 */

import { resolveConsoleScope } from '@/lib/console-scope'
import { resolveServerLocation } from '@/lib/server-navigation'

/**
 * Where a page sits: under Projects, or one of the Server page's tabs
 * (databases, domains, servers), which the Server row stands for.
 */
export type TreeBranchId = 'projects' | 'databases' | 'domains' | 'servers'

/** The branches the Server row stands for. */
export const SERVER_BRANCHES: readonly TreeBranchId[] = [
  'databases',
  'domains',
  'servers',
]

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
 * none: the tree does not claim it is healthy. One with no address to check
 * (a private worker) is read from the health checks its worker reports.
 */
export function serviceState(
  status: string | undefined,
  runtimeStatus?: string | null
): TreeState | undefined {
  if (status === 'no_address')
    switch (runtimeStatus) {
      case 'healthy':
        return state('ok', 'Healthy')
      case 'unhealthy':
        return state('bad', 'Unhealthy')
      default:
        return undefined
    }
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
    case 'dns_configured':
      return state('warn', 'Pending')
    // A service's custom domain another service already serves.
    case 'hostname_conflict':
      return state('bad', 'Conflict')
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

/**
 * What one row says for several records (the Server row for every node and
 * database, a service's Domains row for its domains): trouble counted ("1
 * offline"), since one of several is not all of them; otherwise `healthy`,
 * once anything is known.
 */
export function countedState(
  states: readonly (TreeState | undefined)[],
  healthy = 'Healthy'
): TreeState | undefined {
  const worst = worstState(states)
  if (!worst) return undefined
  if (worst.tone === 'ok' || worst.tone === 'idle') return state('ok', healthy)
  const label = worst.label.replace(/^\d+ /, '').toLowerCase()
  const count = states.filter(
    (candidate) => candidate?.label.toLowerCase() === label
  ).length
  return state(worst.tone, `${count} ${label}`)
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
 * `nodes` with the service shown among them. The tree reads one page of
 * services, so on a large server the one open may be past it: it is added
 * as a row of its own, so its views are still listed under it.
 */
export function withShownService(
  nodes: readonly ProjectNode[],
  shown: ServiceNode | undefined
): ProjectNode[] {
  if (!shown) return [...nodes]
  const listed = nodes.some((node) =>
    node.kind === 'project'
      ? node.services.some((service) => service.id === shown.id)
      : node.service.id === shown.id
  )
  if (listed) return [...nodes]
  return [
    ...nodes,
    { kind: 'service', key: `service:${shown.id}`, service: shown },
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

/** One service whose databases an open project asks for. */
export interface DatabaseRequest {
  /** The project's tree key. */
  key: string
  service: ServiceNode
}

/**
 * The services whose databases are asked for, open project by open
 * project, at most `limit` in all, and the projects left with some not
 * asked for. The tree never asks more than `limit` at once, however many
 * projects are open.
 */
export function databaseRequests(
  projects: readonly { key: string; services: readonly ServiceNode[] }[],
  limit: number
): { requests: DatabaseRequest[]; truncated: Set<string> } {
  const requests: DatabaseRequest[] = []
  const truncated = new Set<string>()
  for (const project of projects)
    for (const service of project.services) {
      if (requests.length >= limit) {
        truncated.add(project.key)
        break
      }
      requests.push({ key: project.key, service })
    }
  return { requests, truncated }
}

/**
 * The projects listed on a server with more than `limit`: the one shown,
 * those in trouble, those opened recently (most recent first), then the
 * rest in their order, up to `limit`. Trouble is listed first; the others
 * keep their order in the full list, so a row does not move as the user
 * opens another. With `limit` or fewer, every project is listed as it is.
 */
export function pickProjects<T extends { key: string }>(
  nodes: readonly T[],
  limit: number,
  rules: {
    isCurrent: (node: T) => boolean
    inTrouble: (node: T) => boolean
    /** Keys of the projects opened recently, most recent first. */
    recent: readonly string[]
  }
): { shown: T[]; more: number } {
  if (nodes.length <= limit) return { shown: [...nodes], more: 0 }
  const chosen = new Set<string>()
  const add = (node: T) => {
    if (chosen.size < limit) chosen.add(node.key)
  }
  nodes.filter(rules.isCurrent).forEach(add)
  nodes.filter(rules.inTrouble).forEach(add)
  const byKey = new Map(nodes.map((node) => [node.key, node]))
  for (const key of rules.recent) {
    const node = byKey.get(key)
    if (node) add(node)
  }
  nodes.forEach(add)
  const picked = nodes.filter((node) => chosen.has(node.key))
  const shown = [
    ...picked.filter(rules.inTrouble),
    ...picked.filter((node) => !rules.inTrouble(node)),
  ]
  return { shown, more: nodes.length - shown.length }
}

/** Projects remembered as opened recently in this browser. */
export const RECENT_PROJECTS = 8

/** `key` first in the recent list, which stays at most `RECENT_PROJECTS`. */
export function withRecent(recent: readonly string[], key: string): string[] {
  return [key, ...recent.filter((other) => other !== key)].slice(
    0,
    RECENT_PROJECTS
  )
}
