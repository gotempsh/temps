// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The fields of `ComposePlacementResponse` the summary reads. */
export interface ComposeHomeFields {
  pinned: boolean
  home_node_name?: string | null
  moving_from_node_id?: number | null
  moving_from_node_name?: string | null
}

/** The fields of `ComposePlacementResponse` a move reads. */
export interface ComposeMoveFields {
  pinned: boolean
  home_node_id?: number | null
  moving_from_node_id?: number | null
}

/**
 * The node running the stack now (`0` is the control plane): the host a
 * pending move is leaving, which keeps running it until the next
 * deployment, or else the home. `null` while the stack has never run.
 */
export function composeRunningNodeId(
  placement: ComposeMoveFields
): number | null {
  if (!placement.pinned) return null
  return placement.moving_from_node_id ?? placement.home_node_id ?? null
}

/**
 * Whether moving to `target` starts the stack without its data. Moving to
 * the node already running it (cancelling a pending move) loses nothing.
 */
export function composeMoveLosesData(
  placement: ComposeMoveFields,
  target: number | null
): boolean {
  const running = composeRunningNodeId(placement)
  return running != null && target !== running
}

/** The node label that marks a build-only node, which hosts no workloads. */
const NODE_ROLE_LABEL = 'temps.sh/role'
const BUILDER_NODE_ROLE = 'builder'

function isBuildOnlyNode(node: { labels?: unknown }): boolean {
  const labels = node.labels
  if (labels == null || typeof labels !== 'object') return false
  return (
    (labels as Record<string, unknown>)[NODE_ROLE_LABEL] === BUILDER_NODE_ROLE
  )
}

/**
 * Nodes a stack can be moved to: every node except its current home, and
 * never a build-only node (`temps.sh/role=builder`), which the server
 * refuses because it hosts no workloads.
 */
export function composeMoveCandidates<
  T extends { id: number; labels?: unknown },
>(placement: ComposeMoveFields, nodes: T[]): T[] {
  return nodes.filter(
    (node) =>
      !isBuildOnlyNode(node) &&
      (!placement.pinned || node.id !== placement.home_node_id)
  )
}

/**
 * One line naming where a Compose stack lives, and one explaining why it
 * stays there. A stack is pinned to the node its first deployment ran on
 * because its volumes are there.
 */
export function composeHomeSummary(placement: ComposeHomeFields): {
  title: string
  detail: string
} {
  if (!placement.pinned) {
    return {
      title: 'Not deployed yet',
      detail:
        'The first deployment runs on a node the scheduler picks within Target Nodes and Label Selectors; with both empty it can be the control plane or any worker. The stack then stays on that node, because its volumes live there.',
    }
  }
  if (placement.moving_from_node_id != null) {
    const running = placement.moving_from_node_name ?? 'an unknown node'
    const destination = placement.home_node_name ?? 'an unknown node'
    return {
      title: `Runs on ${running}, moving to ${destination}`,
      detail: `It keeps running on ${running}, with its volumes, until the next deployment starts it on ${destination} with empty volumes. The data on ${running} stays there.`,
    }
  }
  return {
    title: `Runs on ${placement.home_node_name ?? 'an unknown node'}`,
    detail:
      'The stack stays on this node because its volumes live there. Changing Target Nodes does not move it. Use Move stack to change nodes; its data stays behind.',
  }
}
