import type { SpanRecord } from '@/api/client/types.gen'

/**
 * A span plus its resolved children and tree depth. Shared by the
 * single-project trace waterfall (`TraceDetail`) and the cross-project
 * unified waterfall (`CrossProjectTraceDetail`).
 */
export interface SpanTreeNode {
  span: SpanRecord
  children: SpanTreeNode[]
  depth: number
}

// Build a tree of spans from flat list using parent_span_id
export function buildSpanTree(spans: SpanRecord[]): SpanTreeNode[] {
  const spanMap = new Map<string, SpanTreeNode>()
  const roots: SpanTreeNode[] = []

  // Create nodes
  for (const span of spans) {
    spanMap.set(span.span_id, { span, children: [], depth: 0 })
  }

  // Build tree. Depth cannot be derived here from `parent.depth` because
  // spans can appear in any order relative to their parent (the cross-project
  // unified trace endpoint in particular interleaves spans across projects,
  // so a child frequently precedes its parent in the array) — a parent's
  // depth may still be its default 0 when an earlier-processed child reads
  // it. Depth is assigned in a separate traversal below instead.
  for (const span of spans) {
    const node = spanMap.get(span.span_id)!
    if (span.parent_span_id && spanMap.has(span.parent_span_id)) {
      const parent = spanMap.get(span.parent_span_id)!
      parent.children.push(node)
    } else {
      roots.push(node)
    }
  }

  // Assign depth via traversal from the roots, independent of input order.
  function assignDepth(node: SpanTreeNode, depth: number) {
    node.depth = depth
    node.children.forEach((child) => assignDepth(child, depth + 1))
  }
  roots.forEach((root) => assignDepth(root, 0))

  // Sort children by start_time
  function sortChildren(node: SpanTreeNode) {
    node.children.sort(
      (a, b) =>
        new Date(a.span.start_time).getTime() -
        new Date(b.span.start_time).getTime()
    )
    node.children.forEach(sortChildren)
  }
  roots.sort(
    (a, b) =>
      new Date(a.span.start_time).getTime() -
      new Date(b.span.start_time).getTime()
  )
  roots.forEach(sortChildren)

  return roots
}

// Flatten tree into ordered list for rendering
export function flattenTree(nodes: SpanTreeNode[]): SpanTreeNode[] {
  const result: SpanTreeNode[] = []
  function walk(node: SpanTreeNode) {
    result.push(node)
    node.children.forEach(walk)
  }
  nodes.forEach(walk)
  return result
}
