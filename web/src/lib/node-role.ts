// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Scheduling role of a worker node, read from its `temps.sh/role` label.
 *
 * Mirrors `NODE_ROLE_LABEL` / `BUILDER_NODE_ROLE` / `DEDICATED_NODE_ROLE` in
 * `crates/temps-deployments/src/services/node_scheduler.rs`. The nodes API
 * already returns every label, so the console derives the role instead of
 * the server shipping a second copy of the same fact.
 */
export const NODE_ROLE_LABEL = 'temps.sh/role'

export type NodeSchedulingRole = 'builder' | 'dedicated'

/** The node's scheduling role, or `null` for an ordinary worker. */
export function nodeSchedulingRole(labels: unknown): NodeSchedulingRole | null {
  if (!labels || typeof labels !== 'object' || Array.isArray(labels)) {
    return null
  }
  const role = (labels as Record<string, unknown>)[NODE_ROLE_LABEL]
  return role === 'builder' || role === 'dedicated' ? role : null
}

export const NODE_ROLE_BADGE: Record<
  NodeSchedulingRole,
  { label: string; description: string }
> = {
  builder: {
    label: 'Builder',
    description:
      'Build-only node (temps.sh/role=builder): builds images and never hosts application replicas.',
  },
  dedicated: {
    label: 'Dedicated',
    description:
      'Dedicated node (temps.sh/role=dedicated): only runs environments that pin it in Target Nodes. Unpinned deployments, label selectors and automatic sandbox placement skip it.',
  },
}

/**
 * Help text for the environment's Target Nodes picker. Says what an empty
 * selection means given the nodes that are actually there, so an operator
 * with a dedicated node learns it will not be used unless it is ticked.
 */
export function targetNodesHint(
  nodes: ReadonlyArray<{ labels: unknown }>
): string {
  const hasDedicated = nodes.some(
    (node) => nodeSchedulingRole(node.labels) === 'dedicated'
  )
  if (!hasDedicated) {
    return 'Restrict deployments to specific nodes. Leave empty to use all active nodes.'
  }
  return 'Restrict deployments to specific nodes. Leave empty to use all active nodes except dedicated ones: a dedicated node only runs environments that select it here, and label selectors never match it.'
}
