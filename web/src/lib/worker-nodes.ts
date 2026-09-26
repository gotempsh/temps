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
