// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// What the node page's Sandboxes tab says when it cannot show a list
// (ADR 063). Kept apart from the panel so every state is testable without
// rendering.

import { problemSetupPath } from '@/lib/api-problem'
import {
  isWorkerNodeRequiredProblem,
  sameOriginSetupPath,
  WORKER_NODE_REQUIRED_MESSAGE,
} from '@/lib/worker-nodes'

/** Why the tab could not load, phrased for the operator. */
export type NodeSandboxesFailure =
  | {
      /** Sandboxes need a worker node and none can take them. */
      kind: 'worker-node-required'
      reason: string
      setupPath: string
    }
  | { kind: 'message'; message: string }

/**
 * Turn a failed listing into something the operator can act on. The server's
 * own `detail` is the most specific sentence there is, so it wins. Without
 * one, the response is a bare HTTP status (a proxy, an older server, a route
 * that is not served), and its reason phrase ("Not Found") is never shown
 * on its own: it says nothing about what to do.
 */
export function nodeSandboxesFailure(error: unknown): NodeSandboxesFailure {
  if (isWorkerNodeRequiredProblem(error)) {
    return {
      kind: 'worker-node-required',
      reason: detailOf(error) ?? WORKER_NODE_REQUIRED_MESSAGE,
      setupPath: sameOriginSetupPath(problemSetupPath(error)),
    }
  }
  const detail = detailOf(error)
  if (detail) return { kind: 'message', message: detail }
  return { kind: 'message', message: statusMessage(statusOf(error)) }
}

function statusMessage(status: number | undefined): string {
  if (status === 404) {
    return 'This server does not list the sandboxes on a node. Update Temps on the control plane, then reload this page.'
  }
  if (status === 401) {
    return 'Your session has ended. Sign in again to see the sandboxes on this node.'
  }
  if (status === 403) {
    return 'You are not allowed to see the sandboxes on this node. Ask an administrator.'
  }
  if (status !== undefined && status >= 500) {
    return 'The server failed while listing the sandboxes on this node. Try again in a moment; if it keeps failing, check the Temps server logs.'
  }
  return 'The server could not be reached to list the sandboxes on this node. Check your connection and try again.'
}

function detailOf(error: unknown): string | undefined {
  if (!error || typeof error !== 'object') return undefined
  const detail = (error as { detail?: unknown }).detail
  return typeof detail === 'string' && detail.trim().length > 0
    ? detail
    : undefined
}

function statusOf(error: unknown): number | undefined {
  if (!error || typeof error !== 'object') return undefined
  const status = (error as { status?: unknown }).status
  return typeof status === 'number' ? status : undefined
}

/**
 * Whether the tab is showing the control plane of a server that runs no
 * workloads. `sandboxesLocal` comes from `PlatformFeatures.sandboxes_local`;
 * unknown (still loading, or an older server without the field) is treated
 * as "runs workloads", which is how every server behaved before it.
 */
export function isControlPlaneWithoutWorkloads(
  isControlPlane: boolean,
  sandboxesLocal: boolean | undefined
): boolean {
  return isControlPlane && sandboxesLocal === false
}

/** Shown on the control plane's own node page when it runs no workloads. */
export const CONTROL_PLANE_RUNS_NO_SANDBOXES_MESSAGE =
  'This control plane runs no workloads, so sandboxes never run here; they run on worker nodes.'
