// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Which node(s) the Proxy and Server pages show (ADR 061).
 *
 * The control plane (node 0) and every worker that has joined each write the
 * same `proxy.*` / `node.*` series under their own id. A page shows either one
 * node or "All nodes", which the API combines server-side. An install with
 * only the control plane has nothing to choose: the scope is node 0 and the
 * selector stays hidden, so it looks exactly as it did before workers existed.
 */

import type {
  MetricDataPoint,
  NodeInfoResponse,
  NodeMetricsReportingStatus,
} from '@/api/client/types.gen'
import {
  nodeMetricsGetClusterLatestOptions,
  nodeMetricsGetClusterRangeOptions,
  nodeMetricsGetLatestOptions,
  nodeMetricsGetRangeOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { QueryFunction, QueryKey } from '@tanstack/react-query'

/** The control-plane node always has id 0. */
export const CONTROL_PLANE_NODE_ID = 0

/** URL value (`?node=all`) for every node combined. */
export const ALL_NODES = 'all'

export type NodeOption = { id: number; name: string }

export type NodeScope =
  { kind: 'node'; id: number } | { kind: 'all'; ids: number[] }

/**
 * Nodes that can have metrics: the control plane first, then every worker
 * that has joined (pending ones have no agent yet), by name.
 */
export function metricNodeOptions(
  nodes: NodeInfoResponse[] | undefined
): NodeOption[] {
  const workers = (nodes ?? [])
    .filter((n) => n.id !== CONTROL_PLANE_NODE_ID && n.status !== 'pending')
    .map((n) => ({ id: n.id, name: n.name }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.id - b.id)
  return [{ id: CONTROL_PLANE_NODE_ID, name: 'Control plane' }, ...workers]
}

/**
 * The scope for the `?node=` value. One node only: node 0, whatever the URL
 * says. Several: the node named by id, else all of them.
 */
export function resolveNodeScope(
  param: string | null,
  options: NodeOption[]
): NodeScope {
  if (options.length <= 1) return { kind: 'node', id: CONTROL_PLANE_NODE_ID }
  if (param != null && param !== ALL_NODES && /^\d+$/.test(param)) {
    const id = Number(param)
    if (options.some((o) => o.id === id)) return { kind: 'node', id }
  }
  return { kind: 'all', ids: options.map((o) => o.id) }
}

/** The `?node=` value (and select value) for a scope. */
export function scopeValue(scope: NodeScope): string {
  return scope.kind === 'all' ? ALL_NODES : String(scope.id)
}

/** Whether the scope is the control plane alone (node-0-only panels). */
export function isControlPlaneOnly(scope: NodeScope): boolean {
  return scope.kind === 'node' && scope.id === CONTROL_PLANE_NODE_ID
}

/** Who the page is about, for descriptions: "the control plane", a node name or "all nodes". */
export function scopeSubject(scope: NodeScope, options: NodeOption[]): string {
  if (scope.kind === 'all') return 'all nodes'
  if (scope.id === CONTROL_PLANE_NODE_ID) return 'the control plane'
  return options.find((o) => o.id === scope.id)?.name ?? `node ${scope.id}`
}

type RangeParams = {
  metric: string
  range?: string
  start_time?: string | null
  end_time?: string | null
}

type MetricQuery<T> = {
  queryKey: QueryKey
  queryFn: QueryFunction<T, QueryKey>
}

/** Range query for one metric over the scope. */
export function nodeRangeQuery(
  scope: NodeScope,
  query: RangeParams
): MetricQuery<MetricDataPoint[]> {
  const options =
    scope.kind === 'all'
      ? nodeMetricsGetClusterRangeOptions({
          query: {
            metric: query.metric,
            range: query.range,
            start_time: query.start_time ?? undefined,
            end_time: query.end_time ?? undefined,
            node_ids: scope.ids.join(','),
          },
        })
      : nodeMetricsGetRangeOptions({ path: { id: scope.id }, query })
  return {
    queryKey: options.queryKey,
    queryFn: options.queryFn as unknown as QueryFunction<
      MetricDataPoint[],
      QueryKey
    >,
  }
}

/** Latest-values query over the scope. */
export function nodeLatestQuery(
  scope: NodeScope
): MetricQuery<Record<string, number>> {
  const options =
    scope.kind === 'all'
      ? nodeMetricsGetClusterLatestOptions({
          query: { node_ids: scope.ids.join(',') },
        })
      : nodeMetricsGetLatestOptions({ path: { id: scope.id } })
  return {
    queryKey: options.queryKey,
    queryFn: options.queryFn as unknown as QueryFunction<
      Record<string, number>,
      QueryKey
    >,
  }
}

export type ReportingNotice = { title: string; detail: string }

const TOO_OLD = 'agent_too_old'

/**
 * What to say about workers in the scope that are not reporting. Nothing for
 * the control plane, for a worker that reports, or for one the control plane
 * has not pulled yet.
 */
export function reportingNotice(
  scope: NodeScope,
  options: NodeOption[],
  statuses: NodeMetricsReportingStatus[]
): ReportingNotice | null {
  const nameOf = (id: number) =>
    options.find((o) => o.id === id)?.name ?? `node ${id}`
  const failing = statuses.filter(
    (s) => s.state !== 'reporting' && s.node_id !== CONTROL_PLANE_NODE_ID
  )

  if (scope.kind === 'node') {
    const status = failing.find((s) => s.node_id === scope.id)
    if (!status) return null
    if (status.state === TOO_OLD) {
      return {
        title: 'Agent too old to report metrics',
        detail: `The agent on ${nameOf(status.node_id)} predates worker metrics. Upgrade it and its proxy and host metrics appear here within one scrape interval.`,
      }
    }
    return {
      title: `No metrics arriving from ${nameOf(status.node_id)}`,
      detail:
        status.detail ??
        'The control plane could not read this node’s metrics.',
    }
  }

  const inScope = failing.filter((s) => scope.ids.includes(s.node_id))
  if (inScope.length === 0) return null
  const tooOld = inScope.filter((s) => s.state === TOO_OLD)
  const other = inScope.filter((s) => s.state !== TOO_OLD)
  const names = (list: NodeMetricsReportingStatus[]) =>
    list.map((s) => nameOf(s.node_id)).join(', ')
  const parts: string[] = []
  if (tooOld.length > 0) {
    parts.push(
      `${names(tooOld)}: agent too old to report metrics — upgrade it to include ${tooOld.length === 1 ? 'it' : 'them'}.`
    )
  }
  if (other.length > 0) {
    parts.push(`${names(other)}: not reporting right now.`)
  }
  return {
    title: `${inScope.length} of ${scope.ids.length} nodes not included`,
    detail: parts.join(' '),
  }
}
