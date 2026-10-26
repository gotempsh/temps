// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// The generated type, imported directly rather than through the hook: the
// hook imports the poll policy from this module, and routing the type back
// through it would make that a cycle.
import type { NodeCapabilityResponse as NodeCapability } from '@/api/client/types.gen'

/**
 * Console route for the Worker Nodes page.
 *
 * The page has always lived under `/settings/nodes` and keeps that URL so
 * existing links, breadcrumbs and bookmarks stay valid — but it is surfaced
 * in the main "Build & deliver" navigation, not in Settings.
 */
export const WORKER_NODES_URL = '/settings/nodes'

/**
 * Anchors for the cards in the Worker Nodes page's collapsed "Advanced"
 * section. Linking to one (`/settings/nodes#cluster-dns`) opens the section
 * and scrolls to that card, so search and other pages can point at it.
 */
export const NODES_ADVANCED_SECTIONS = [
  'mesh-hub',
  'cluster-network',
  'cluster-dns',
  'cluster-trust',
] as const

export type NodesAdvancedSection = (typeof NODES_ADVANCED_SECTIONS)[number]

/** The advanced card a location hash points at, if any. */
export function nodesAdvancedSection(
  hash: string
): NodesAdvancedSection | null {
  const id = hash.replace(/^#/, '')
  return (NODES_ADVANCED_SECTIONS as readonly string[]).includes(id)
    ? (id as NodesAdvancedSection)
    : null
}

/** The Cluster DNS settings card (`*.temps.local` resolution). */
export const CLUSTER_DNS_URL = `${WORKER_NODES_URL}#cluster-dns`

/** Public documentation for joining a worker node (`temps join`). */
export const WORKER_NODES_DOCS_URL = 'https://temps.sh/docs/multi-node'

/** Error code the API returns when an action needs a worker node to run on. */
export const WORKER_NODE_REQUIRED_ERROR_CODE = 'WORKER_NODE_REQUIRED'

export const WORKER_NODE_REQUIRED_TITLE =
  'No machine available to run workloads'

export const WORKER_NODE_REQUIRED_MESSAGE =
  'This control plane runs no local workloads. Add a worker node to run builds, deployments and managed services.'

/**
 * What a user who cannot add a node is told instead of being handed a button.
 *
 * The Worker Nodes page needs `SettingsRead` to list the inventory and
 * `SettingsWrite` to mint an enrollment token, so sending a project user there
 * produces "Failed to load worker nodes" — an advertised remedy that denies
 * the person who followed it. They still need to know *why* nothing deploys,
 * so the explanation stays and only the action changes.
 */
export const WORKER_NODE_ASK_ADMIN_MESSAGE =
  'Ask an administrator to add a worker node.'

/**
 * Whether this user can act on the remedy the banner advertises.
 *
 * Server-computed (`can_manage_nodes`) rather than inferred client-side: the
 * console cannot know what a custom role grants. Absent — an older server, or
 * a capability that has not loaded — is treated as "cannot", so a dead link is
 * never the default.
 */
export function canAddWorkerNode(
  capability: NodeCapability | undefined | null
): boolean {
  return capability?.can_manage_nodes === true
}

/** How often to re-read the capability, in ms, or `false` for not at all. */
export const NODE_CAPABILITY_POLL_MS = 30_000

/**
 * Poll the capability only while nothing can run.
 *
 * Unschedulable is the one state that resolves from *outside* the console — a
 * `temps join` on another machine — so without polling the banner outlives the
 * problem until the page is remounted (focus refetching is disabled globally).
 * Once something can run, the state only changes through actions the console
 * already invalidates on, so the poll stops rather than burning a request
 * every 30 s on every mounted page forever.
 */
export function nodeCapabilityRefetchInterval(
  capability: NodeCapability | undefined | null
): number | false {
  return capability?.schedulable === false ? NODE_CAPABILITY_POLL_MS : false
}

/**
 * Whether the "add a worker node" banner should be rendered.
 *
 * Deliberately fails *closed* (hidden) while the capability is unknown —
 * `undefined` covers both "still loading" and "this server predates the
 * capability endpoint", and telling an operator their platform cannot run
 * anything because a request has not come back yet would be worse than
 * saying nothing.
 */
export function shouldShowWorkerNodeBanner(
  capability: NodeCapability | undefined | null
): boolean {
  return capability != null && capability.schedulable === false
}

/**
 * Whether the Nodes page should present its empty state as an onboarding
 * prompt rather than the informational "everything runs here" copy: no worker
 * has joined *and* the control plane cannot run workloads itself.
 */
export function shouldPromptForFirstWorkerNode(
  capability: NodeCapability | undefined | null,
  nodeCount: number
): boolean {
  return nodeCount === 0 && capability != null && !capability.local_workloads
}

/** Extract the `error_code` an RFC 7807 Problem carries, if any. */
export function problemErrorCode(error: unknown): string | undefined {
  if (!error || typeof error !== 'object') return undefined
  const problem = error as {
    error_code?: unknown
    extensions?: { error_code?: unknown } | null
  }
  if (typeof problem.error_code === 'string') return problem.error_code
  const extension = problem.extensions?.error_code
  return typeof extension === 'string' ? extension : undefined
}

/** Containers a node removal could not confirm are gone. */
export interface UnremovedNodeContainers {
  /** How many there are. */
  count: number
  /**
   * Their descriptions. The API describes at most 50 and ends with a line
   * counting the rest, so this can be shorter than `count`.
   */
  lines: string[]
}

/**
 * The containers a node removal could not confirm are gone, when the API
 * refused it for that reason (409 "Node Still Holds Containers"). Only then
 * is retrying with `force=true` the operator's call to make.
 */
export function unremovedNodeContainers(
  error: unknown
): UnremovedNodeContainers | undefined {
  if (!error || typeof error !== 'object') return undefined
  const problem = error as {
    unremoved_containers?: unknown
    unremoved_count?: unknown
    extensions?: {
      unremoved_containers?: unknown
      unremoved_count?: unknown
    } | null
  }
  const list =
    problem.unremoved_containers ?? problem.extensions?.unremoved_containers
  if (!Array.isArray(list) || list.length === 0) return undefined
  const lines = list.filter((item): item is string => typeof item === 'string')
  const count = problem.unremoved_count ?? problem.extensions?.unremoved_count
  return {
    count:
      typeof count === 'number' && Number.isInteger(count) && count > 0
        ? count
        : lines.length,
    lines,
  }
}

/**
 * Seconds without a heartbeat after which the control plane treats a node as
 * unreachable; matches `NODE_HEARTBEAT_STALE_SECS` on the server.
 */
export const NODE_HEARTBEAT_STALE_SECS = 90

/**
 * Whether the control plane has stopped hearing from a node: never heard
 * from, or silent past the threshold. Decided on the heartbeat alone, like
 * the server: a node that was draining when its agent stopped keeps its
 * status (ADR 057).
 */
export function nodeIsUnreachable(
  node: { last_heartbeat?: string | null },
  now: number = Date.now()
): boolean {
  if (!node.last_heartbeat) return true
  const heartbeat = Date.parse(node.last_heartbeat)
  if (Number.isNaN(heartbeat)) return false
  return now - heartbeat > NODE_HEARTBEAT_STALE_SECS * 1000
}

/** What a removal of an unreachable node would take down. */
export interface ServingOnUnreachableNode {
  /** Containers of live deployments recorded on the node. */
  count: number
  /** The environments they serve, as the API describes them. */
  environments: string[]
}

/**
 * The live deployments an unreachable node still serves, when the API
 * refused its removal for that reason (409 "Unreachable Node Still Serves
 * Deployments"). Only then does retrying with `force=true` remove it anyway,
 * and those environments stop until they are redeployed.
 */
export function servingOnUnreachableNode(
  error: unknown
): ServingOnUnreachableNode | undefined {
  if (!error || typeof error !== 'object') return undefined
  type Fields = {
    force_removes_serving?: unknown
    serving_containers?: unknown
    serving_environments?: unknown
  }
  const problem = error as Fields & { extensions?: Fields | null }
  const field = <K extends keyof Fields>(key: K) =>
    problem[key] ?? problem.extensions?.[key]
  if (field('force_removes_serving') !== true) return undefined
  const list = field('serving_environments')
  const environments = Array.isArray(list)
    ? list.filter((item): item is string => typeof item === 'string')
    : []
  const count = field('serving_containers')
  return {
    count:
      typeof count === 'number' && Number.isInteger(count) && count > 0
        ? count
        : environments.length,
    environments,
  }
}

/** True when a failed request failed because no worker node can run it. */
export function isWorkerNodeRequiredProblem(error: unknown): boolean {
  return problemErrorCode(error) === WORKER_NODE_REQUIRED_ERROR_CODE
}

/**
 * A `setup_path` from a Problem body is only ever followed when it is a
 * plain same-origin path. The server's value is a fixed constant today, but
 * this handler acts on any Problem that carries the field, so it must not
 * become an open redirect if some future emitter gets it wrong.
 */
export function sameOriginSetupPath(path: string | undefined): string {
  if (!path || !path.startsWith('/') || path.startsWith('//'))
    return WORKER_NODES_URL
  if (/[^\x21-\x7e]|\\/.test(path)) return WORKER_NODES_URL
  return path
}

/** The fields of a node the removal rule reads. */
export interface NodeRemovalFacts {
  id: number
  role: string
  status: string
  last_heartbeat?: string | null
  serving_containers?: number | null
  live_sandboxes?: number | null
}

/**
 * What the console offers for removing a node.
 *
 * - `none`: no removal action (the control plane's own entry, or a state such
 *   as `pending` the server would not remove directly).
 * - `remove`: the node can be removed in one step. For an unreachable or
 *   offline node the server may still refuse and offer a forced removal.
 * - `drain-first`: a reachable node still runs deployment containers; a drain
 *   moves them (or is already moving them, when `draining`).
 * - `sandboxes`: the node hosts sandboxes, which draining does not move; they
 *   have to be destroyed first.
 * - `unknown`: the server predates `serving_containers`/`live_sandboxes`, so
 *   callers keep their previous behaviour: the node detail page decides from
 *   the drain status, and the node list offers no action.
 */
export type NodeRemoval =
  | { kind: 'none' }
  | { kind: 'remove' }
  | { kind: 'drain-first'; containers: number; draining: boolean }
  | { kind: 'sandboxes'; sandboxes: number }
  | { kind: 'unknown' }

/** The control plane's id in the node list (`CONTROL_PLANE_NODE_ID`). */
export const CONTROL_PLANE_NODE_ID = 0

/**
 * Whether, and how, a node can be removed, from what the node list reports.
 *
 * Mirrors what `DELETE /internal/nodes/{id}` accepts: a reachable node is
 * removed directly once nothing serves from it and it hosts no sandboxes; an
 * unreachable or offline node can be removed whatever it ran (a drain cannot
 * finish without its agent, ADR 057), with the server asking before it
 * forcibly removes containers still serving. Sandboxes block either way.
 */
export function nodeRemoval(
  node: NodeRemovalFacts,
  now: number = Date.now()
): NodeRemoval {
  if (node.role === 'control-plane' || node.id === CONTROL_PLANE_NODE_ID) {
    return { kind: 'none' }
  }
  const serving = node.serving_containers
  const sandboxes = node.live_sandboxes
  if (serving == null || sandboxes == null) return { kind: 'unknown' }
  // Sandboxes block every removal, forced or not, and a drain never moves
  // them.
  if (sandboxes > 0) return { kind: 'sandboxes', sandboxes }
  if (nodeIsUnreachable(node, now) || node.status === 'offline') {
    return { kind: 'remove' }
  }
  if (serving > 0) {
    return {
      kind: 'drain-first',
      containers: serving,
      draining: node.status === 'draining',
    }
  }
  return node.status === 'active' ||
    node.status === 'draining' ||
    node.status === 'drained'
    ? { kind: 'remove' }
    : { kind: 'none' }
}

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`
}

/**
 * Why a node cannot be removed yet, in one short line, or `null` when nothing
 * stands in the way (or the rule has nothing to say).
 */
export function nodeRemovalBlocker(removal: NodeRemoval): string | null {
  switch (removal.kind) {
    case 'drain-first':
      return removal.draining
        ? `Draining — ${plural(removal.containers, 'container', 'containers')} left`
        : `Drain first — it has ${plural(removal.containers, 'container', 'containers')}`
    case 'sandboxes':
      return `Hosts ${plural(removal.sandboxes, 'sandbox', 'sandboxes')} — destroy ${removal.sandboxes === 1 ? 'it' : 'them'} first (draining does not move sandboxes)`
    default:
      return null
  }
}
