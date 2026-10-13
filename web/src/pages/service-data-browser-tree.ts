// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pure helpers for the service data browser tree.
 *
 * Kept out of `ServiceDataBrowser.tsx` so the rules for "what may I ask the
 * backend for when this node is expanded?" can be unit tested without
 * rendering the page.
 */

export interface PathNode {
  path: string
  children?: PathNode[]
}

export interface NodeListingCapabilities {
  canContainContainers?: boolean
  canContainEntities?: boolean
}

/** What the explorer hierarchy says a container at one level can list. */
export interface HierarchyLevelCapabilities {
  can_list_containers: boolean
  can_list_entities: boolean
}

export interface NodeListing {
  /** Request child containers (`.../query/containers/{path}`). */
  listContainers: boolean
  /** Request entities (`.../query/containers/{path}/entities`). */
  listEntities: boolean
}

export function findTreeNode<T extends PathNode>(
  nodes: T[],
  path: string
): T | null {
  for (const node of nodes) {
    if (node.path === path) return node
    if (node.children) {
      const found = findTreeNode(node.children as T[], path)
      if (found) return found
    }
  }
  return null
}

/** 1-based hierarchy level of a container path (`db` = 1, `db/schema` = 2). */
export function containerPathLevel(path: string): number {
  return path.split('/').length
}

/**
 * Decide which child lists to request when `node` is expanded.
 *
 * Capabilities the backend recorded on the node win. When the node is not in
 * the tree snapshot the caller holds (a lookup against stale state, which is
 * how a PostgreSQL schema ended up being asked for child containers -> the
 * backend answered 4xx/5xx), fall back to the hierarchy level of the path
 * instead of assuming the node can contain everything.
 */
export function resolveNodeListing(
  node: NodeListingCapabilities | null,
  levelFallback: HierarchyLevelCapabilities
): NodeListing {
  return {
    listContainers:
      node?.canContainContainers ?? levelFallback.can_list_containers,
    listEntities: node?.canContainEntities ?? levelFallback.can_list_entities,
  }
}

/**
 * A 400 from a navigation request means "this path has nothing of that kind"
 * (e.g. child containers of a leaf container). It is an expected, recoverable
 * answer while probing the tree, not something to log as a failure.
 */
export function isExpectedNavigationRejection(error: unknown): boolean {
  if (typeof error !== 'object' || error === null) return false
  return (error as { status?: unknown }).status === 400
}
