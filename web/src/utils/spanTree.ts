// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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

  function sortChildren(node: SpanTreeNode) {
    node.children.sort(compareSpans)
    node.children.forEach(sortChildren)
  }
  roots.sort(compareSpans)
  roots.forEach(sortChildren)

  return roots
}

/**
 * A span's duration in milliseconds, preferring the server-computed
 * `duration_ms` over subtracting the timestamps.
 *
 * The timestamps are the wrong source: `Date.getTime()` is integer
 * milliseconds, and the API serialises span start/end at millisecond
 * resolution anyway (ClickHouse stores them as `DateTime64(3)`). Subtracting
 * them reports every sub-millisecond span as 0 and rounds a 1.6ms span to 1ms.
 * `duration_ms` is a separate float carrying the original nanosecond
 * precision, so it is exact.
 *
 * Falls back to the timestamps for records that predate the field or are
 * hand-built, which is lossy but better than a zero-width bar.
 */
export function spanDurationMs(span: SpanRecord): number {
  if (typeof span.duration_ms === 'number' && Number.isFinite(span.duration_ms)) {
    return span.duration_ms
  }
  return (
    new Date(span.end_time).getTime() - new Date(span.start_time).getTime()
  )
}

/**
 * The time window a set of spans covers, for positioning waterfall bars.
 *
 * The end is derived from `duration_ms` rather than `end_time` for the same
 * reason bar widths are: `end_time` is truncated to the millisecond, so a
 * window built from it disagrees with the bars drawn inside it — the header
 * would read 225.0ms above a root span labelled 224.8ms.
 */
export function traceWindow(spans: SpanRecord[]): {
  start: number
  end: number
  duration: number
} {
  if (spans.length === 0) return { start: 0, end: 0, duration: 0 }

  let start = Infinity
  let end = -Infinity
  for (const span of spans) {
    const spanStart = new Date(span.start_time).getTime()
    if (spanStart < start) start = spanStart
    const spanEnd = spanStart + spanDurationMs(span)
    if (spanEnd > end) end = spanEnd
  }
  return { start, end, duration: end - start }
}

/**
 * Order two sibling spans as they executed.
 *
 * Start time is the primary key, but it is only accurate to the millisecond,
 * and a parent frequently kicks off several children inside one — so ties are
 * the common case, not the edge case. `Array.sort` is stable, which means an
 * unbroken tie silently inherits whatever order storage returned (ClickHouse
 * orders by `span_id`), which put a transaction body above the
 * before-transaction hook that ran ahead of it.
 *
 * End time breaks the tie, and unlike start it has sub-millisecond resolution
 * because it is derived from `duration_ms`. It is also the right signal: for
 * two siblings that ran sequentially to both start within the same
 * millisecond, the first one must have finished inside that millisecond, so
 * the earlier finisher is the earlier starter. Concurrent siblings have no
 * true order to recover, and `span_id` finally makes the result deterministic
 * so the same trace always renders identically.
 */
function compareSpans(a: SpanTreeNode, b: SpanTreeNode): number {
  const startA = new Date(a.span.start_time).getTime()
  const startB = new Date(b.span.start_time).getTime()
  if (startA !== startB) return startA - startB

  const endA = startA + spanDurationMs(a.span)
  const endB = startB + spanDurationMs(b.span)
  if (endA !== endB) return endA - endB

  return a.span.span_id.localeCompare(b.span.span_id)
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
