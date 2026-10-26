// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type {
  NodeInfoResponse,
  NodeMetricsReportingStatus,
} from '@/api/client/types.gen'
import {
  ALL_NODES,
  isControlPlaneOnly,
  metricNodeOptions,
  nodeLatestQuery,
  nodeRangeQuery,
  reportingNotice,
  resolveNodeScope,
  scopeValue,
} from './node-scope'

const node = (id: number, name: string, status = 'active') =>
  ({ id, name, status }) as NodeInfoResponse

const status = (
  node_id: number,
  state: string,
  detail: string | null = null
): NodeMetricsReportingStatus => ({
  node_id,
  state,
  detail,
  last_attempt_at: '2026-10-23T19:40:00Z',
  last_report_at: null,
})

describe('metricNodeOptions', () => {
  test('a single-node install has only the control plane', () => {
    expect(metricNodeOptions(undefined)).toEqual([
      { id: 0, name: 'Control plane' },
    ])
    expect(metricNodeOptions([node(0, 'control-plane')])).toEqual([
      { id: 0, name: 'Control plane' },
    ])
  })

  test('workers follow the control plane by name; pending ones are left out', () => {
    expect(
      metricNodeOptions([
        node(3, 'zeta'),
        node(0, 'control-plane'),
        node(2, 'alpha'),
        node(4, 'new', 'pending'),
        node(5, 'gone', 'offline'),
      ]).map((o) => o.name)
    ).toEqual(['Control plane', 'alpha', 'gone', 'zeta'])
  })
})

describe('resolveNodeScope', () => {
  const single = metricNodeOptions([])
  const multi = metricNodeOptions([node(0, 'cp'), node(2, 'w1'), node(7, 'w2')])

  test('only the control plane: node 0 whatever the URL says', () => {
    for (const param of [null, ALL_NODES, '2', 'junk']) {
      const scope = resolveNodeScope(param, single)
      expect(scope).toEqual({ kind: 'node', id: 0 })
      expect(isControlPlaneOnly(scope)).toBe(true)
    }
  })

  test('several nodes default to all of them', () => {
    expect(resolveNodeScope(null, multi)).toEqual({
      kind: 'all',
      ids: [0, 2, 7],
    })
    expect(scopeValue(resolveNodeScope(null, multi))).toBe(ALL_NODES)
  })

  test('a known node id picks that node; anything else falls back to all', () => {
    expect(resolveNodeScope('7', multi)).toEqual({ kind: 'node', id: 7 })
    expect(resolveNodeScope('0', multi)).toEqual({ kind: 'node', id: 0 })
    for (const param of ['9', '-1', '2.5', '2abc', '']) {
      expect(resolveNodeScope(param, multi).kind).toBe('all')
    }
  })
})

describe('queries', () => {
  test('one node reads its own series; all nodes read the combined ones', () => {
    const one = nodeRangeQuery({ kind: 'node', id: 2 }, { metric: 'm' })
    expect(JSON.stringify(one.queryKey)).toContain('nodeMetricsGetRange')
    expect(JSON.stringify(one.queryKey)).toContain('"id":2')

    const all = nodeRangeQuery({ kind: 'all', ids: [0, 2] }, { metric: 'm' })
    expect(JSON.stringify(all.queryKey)).toContain('nodeMetricsGetClusterRange')
    expect(JSON.stringify(all.queryKey)).toContain('"node_ids":"0,2"')

    expect(
      JSON.stringify(nodeLatestQuery({ kind: 'all', ids: [0, 2] }).queryKey)
    ).toContain('nodeMetricsGetClusterLatest')
  })
})

describe('reportingNotice', () => {
  const options = metricNodeOptions([
    node(0, 'cp'),
    node(2, 'w1'),
    node(7, 'w2'),
  ])

  test('a worker whose agent is too old says so', () => {
    const notice = reportingNotice({ kind: 'node', id: 2 }, options, [
      status(2, 'agent_too_old', 'HTTP 404'),
    ])
    expect(notice?.title).toBe('Agent too old to report metrics')
    expect(notice?.detail).toContain('w1')
  })

  test('nothing to say for a reporting worker, the control plane, or no status yet', () => {
    expect(
      reportingNotice({ kind: 'node', id: 2 }, options, [
        status(2, 'reporting'),
      ])
    ).toBeNull()
    expect(reportingNotice({ kind: 'node', id: 0 }, options, [])).toBeNull()
    expect(reportingNotice({ kind: 'node', id: 7 }, options, [])).toBeNull()
  })

  test('other failures carry the control plane’s detail', () => {
    const notice = reportingNotice({ kind: 'node', id: 7 }, options, [
      status(7, 'unreachable', 'The agent could not be reached: timeout'),
    ])
    expect(notice?.title).toContain('w2')
    expect(notice?.detail).toContain('timeout')
  })

  test('all nodes lists who is left out and why', () => {
    const notice = reportingNotice({ kind: 'all', ids: [0, 2, 7] }, options, [
      status(2, 'agent_too_old'),
      status(7, 'unreachable'),
    ])
    expect(notice?.title).toBe('2 of 3 nodes not included')
    expect(notice?.detail).toContain('w1: agent too old')
    expect(notice?.detail).toContain('w2: not reporting')
  })
})
