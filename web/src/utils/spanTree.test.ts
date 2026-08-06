import { describe, expect, test } from 'bun:test'
import type { SpanRecord } from '@/api/client/types.gen'
import { buildSpanTree, flattenTree, spanDurationMs } from './spanTree'
import fixture from './__fixtures__/galachain-trace.json'

const spans = fixture.spans as unknown as SpanRecord[]

/** Names of a node's children, in render order. */
function childNames(spans: SpanRecord[], parentSpanId: string): string[] {
  const node = flattenTree(buildSpanTree(spans)).find(
    (n) => n.span.span_id === parentSpanId
  )
  if (!node) throw new Error(`span ${parentSpanId} not in tree`)
  return node.children.map((c) => c.span.name)
}

describe('buildSpanTree ordering', () => {
  test('orders siblings that share a start millisecond by when they finished', () => {
    // The backend serialises start_time at millisecond resolution, so
    // `fabric.beforeTransaction` (0.012ms) and `GalaTransaction.EVALUATE`
    // (7.1ms) both report 12:25:45.202Z. Sorting on start alone leaves the
    // order to `Array.sort`'s stability, i.e. to whatever order storage
    // happened to return — which put the 7ms transaction body *before* the
    // hook that runs first.
    expect(childNames(spans, '12a59f2def9dbd48')).toEqual([
      'fabric.beforeTransaction',
      'GalaTransaction.EVALUATE',
      'fabric.afterTransaction',
    ])

    // Same tie, one level down: authorization runs before the handler.
    expect(childNames(spans, '136d369c2d874860')).toEqual([
      'gala.authorize',
      'gala.handle',
    ])
  })

  test('keeps ordering by start time when starts genuinely differ', () => {
    // gala.handle's four state reads are milliseconds apart, so the end-time
    // tie-break must not disturb them: the 2.1ms read really does come first.
    expect(childNames(spans, '943b45f360f5f6f0')).toEqual([
      'state.getObjectsByPartialCompositeKeyWithPagination',
      'state.getObjectsByKeys',
      'state.getObjectByKey',
      'state.getObjectByKey',
    ])
  })

  test('is stable regardless of the order storage returns spans in', () => {
    // ClickHouse returns spans ordered by span_id, TimescaleDB by start_time.
    // The rendered tree must not depend on which one answered.
    const reversed = [...spans].reverse()
    const forward = flattenTree(buildSpanTree(spans)).map((n) => n.span.span_id)
    const backward = flattenTree(buildSpanTree(reversed)).map(
      (n) => n.span.span_id
    )
    expect(backward).toEqual(forward)
  })

  test('nests every span under its parent', () => {
    const flat = flattenTree(buildSpanTree(spans))
    expect(flat).toHaveLength(spans.length)

    const depthOf = new Map(flat.map((n) => [n.span.span_id, n.depth]))
    for (const node of flat) {
      const parentId = node.span.parent_span_id
      if (!parentId) {
        expect(node.depth).toBe(0)
        continue
      }
      expect(depthOf.get(parentId)).toBe(node.depth - 1)
    }
  })
})

describe('spanDurationMs', () => {
  test('preserves sub-millisecond durations', () => {
    // Recomputing from the timestamps truncates to whole milliseconds, which
    // reported every sub-millisecond span in this trace as "0µs".
    const byName = (name: string) =>
      spans.find((s) => s.name === name && s.duration_ms < 1)!

    expect(spanDurationMs(byName('state.getObjectsByKeys'))).toBeCloseTo(
      0.872704,
      6
    )
    expect(spanDurationMs(byName('fabric.beforeTransaction'))).toBeCloseTo(
      0.012544,
      6
    )
  })

  test('does not round away precision on multi-millisecond spans', () => {
    const getObjectByKey = spans.find(
      (s) => s.name === 'state.getObjectByKey' && s.duration_ms > 1.5
    )!
    // Timestamp subtraction gave 1 here, rendering "1.0ms" for a 1.6ms span.
    expect(spanDurationMs(getObjectByKey)).toBeCloseTo(1.623296, 6)
  })

  test('falls back to the timestamps when duration_ms is absent', () => {
    // Older stored spans, and any caller passing a hand-built record, have no
    // duration_ms. Those must still render a bar rather than collapse to zero.
    const withoutDuration = {
      ...spans[0],
      duration_ms: undefined,
    } as unknown as SpanRecord
    expect(spanDurationMs(withoutDuration)).toBe(225)
  })
})
