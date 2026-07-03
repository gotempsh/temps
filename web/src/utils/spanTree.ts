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

  // Build tree
  for (const span of spans) {
    const node = spanMap.get(span.span_id)!
    if (span.parent_span_id && spanMap.has(span.parent_span_id)) {
      const parent = spanMap.get(span.parent_span_id)!
      node.depth = parent.depth + 1
      parent.children.push(node)
    } else {
      roots.push(node)
    }
  }

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
