// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Settings → DNS (ADR-095): data access for the internal resolver's health,
 * records, query log and traffic, and the pure helpers the page renders
 * with. Everything goes through the generated SDK.
 */

import type { StatusTone } from '@temps-sdk/ds'
import { z } from 'zod'
import {
  dnsTraffic,
  listDnsQueries,
  listDnsRecords,
  listDnsResolvers,
  resolveDnsName,
} from '@/api/client'
import type {
  ClusterDnsResolversResponse,
  DnsQueryLogEntry,
  DnsQueryLogResponse,
  DnsRecordEntry,
  DnsRecordListResponse,
  DnsResolveRequest,
  DnsResolveResponse,
  DnsResolverEntry,
  DnsTrafficBucket,
  DnsTrafficResponse,
  InternalProxyState,
  ListDnsQueriesData,
  ListDnsRecordsData,
  ResolverHealth,
} from '@/api/client'

export type {
  ClusterDnsResolversResponse,
  DnsQueryLogEntry,
  DnsQueryLogResponse,
  DnsRecordEntry,
  DnsRecordListResponse,
  DnsResolveRequest,
  DnsResolveResponse,
  DnsResolverEntry,
  DnsTrafficBucket,
  DnsTrafficResponse,
  InternalProxyState,
  ResolverHealth,
}

/** The control plane's resolver reports as node 0. */
export const CONTROL_PLANE_NODE_ID = 0

export const DNS_OUTCOMES = [
  'noerror',
  'nodata',
  'nxdomain',
  'servfail',
  'refused',
  'error',
] as const
export type DnsOutcome = (typeof DNS_OUTCOMES)[number]

export const DNS_ZONES = ['internal', 'external'] as const
export type DnsZone = (typeof DNS_ZONES)[number]

export const DNS_TRAFFIC_RANGES = ['1h', '6h', '24h', '7d'] as const
export type DnsTrafficRange = (typeof DNS_TRAFFIC_RANGES)[number]

export const DNS_TEST_RECORD_TYPES = [
  'A',
  'AAAA',
  'CNAME',
  'SRV',
  'TXT',
] as const
export type DnsTestRecordType = (typeof DNS_TEST_RECORD_TYPES)[number]

/** `service_endpoints.owner_kind` values, with the label the filter shows. */
/** RFC 1035 lengths; the server validates the same and says why. */
export const testResolutionSchema = z.object({
  node_id: z.number().int().min(0),
  name: z
    .string()
    .trim()
    .min(1, 'Enter a name to resolve')
    .max(253, 'A DNS name is at most 253 characters')
    .regex(
      /^[A-Za-z0-9_.-]+\.?$/,
      'Use letters, digits, hyphens, underscores and dots'
    ),
  record_type: z.enum(DNS_TEST_RECORD_TYPES),
  project_id: z.number().int().positive().nullable(),
})
export type TestResolutionValues = z.infer<typeof testResolutionSchema>

export const DNS_RECORD_OWNER_KINDS = [
  { value: 'environment', label: 'Environments' },
  { value: 'service_member', label: 'Database members' },
  { value: 'service_role', label: 'Database roles' },
  { value: 'deployment', label: 'Deployments' },
  { value: 'node', label: 'Nodes' },
  { value: 'static', label: 'Static' },
] as const

export interface DnsQueriesParams {
  page: number
  page_size: number
  node_id?: number
  outcome?: DnsOutcome
  zone?: DnsZone
  name?: string
}

export interface DnsRecordsParams {
  page: number
  page_size: number
  search?: string
  node_id?: number
  owner_kind?: string
}

export const dnsKeys = {
  all: ['cluster-dns'] as const,
  resolvers: ['cluster-dns', 'resolvers'] as const,
  records: (params: DnsRecordsParams) =>
    ['cluster-dns', 'records', params] as const,
  queries: (params: DnsQueriesParams) =>
    ['cluster-dns', 'queries', params] as const,
  traffic: (nodeId: number, range: DnsTrafficRange) =>
    ['cluster-dns', 'traffic', nodeId, range] as const,
}

/** Drop empty filters so the request carries only what narrows it. */
function compact<T extends object>(params: T): T {
  return Object.fromEntries(
    Object.entries(params).filter(
      ([, value]) => value !== undefined && value !== ''
    )
  ) as T
}

export async function getDnsResolvers(): Promise<ClusterDnsResolversResponse> {
  const { data } = await listDnsResolvers({ throwOnError: true })
  return data
}

export async function getDnsRecords(
  params: DnsRecordsParams
): Promise<DnsRecordListResponse> {
  const { data } = await listDnsRecords({
    query: compact(params) as NonNullable<ListDnsRecordsData['query']>,
    throwOnError: true,
  })
  return data
}

export async function getDnsQueries(
  params: DnsQueriesParams
): Promise<DnsQueryLogResponse> {
  const { data } = await listDnsQueries({
    query: compact(params) as NonNullable<ListDnsQueriesData['query']>,
    throwOnError: true,
  })
  return data
}

export async function getDnsTraffic(
  nodeId: number,
  range: DnsTrafficRange
): Promise<DnsTrafficResponse> {
  const { data } = await dnsTraffic({
    query: { node_id: nodeId, range },
    throwOnError: true,
  })
  return data
}

export async function testDnsResolution(
  request: DnsResolveRequest
): Promise<DnsResolveResponse> {
  const { data } = await resolveDnsName({ body: request, throwOnError: true })
  return data
}

// ── Display ─────────────────────────────────────────────────────────────────

interface HealthMeta {
  label: string
  tone: StatusTone
}

const HEALTH_META: Record<ResolverHealth, HealthMeta> = {
  healthy: { label: 'Healthy', tone: 'ok' },
  stale: { label: 'Stale records', tone: 'warn' },
  down: { label: 'Down', tone: 'error' },
  off: { label: 'Off', tone: 'idle' },
  node_unavailable: { label: 'Node unavailable', tone: 'idle' },
  agent_too_old: { label: 'Agent too old', tone: 'warn' },
  not_reporting: { label: 'Not reporting', tone: 'warn' },
}

export function resolverHealthMeta(health: ResolverHealth): HealthMeta {
  return HEALTH_META[health]
}

/**
 * A worker's internal edge proxy, which serves the HTTP behind its
 * `*.temps.local` names. The control plane has none (`null`): its main
 * proxy does this.
 */
export function internalProxyMeta(
  proxy: InternalProxyState | null | undefined
): { label: string; tone: StatusTone } | null {
  if (!proxy) return null
  if (proxy.listening === true) return { label: 'Listening', tone: 'ok' }
  if (proxy.listening === false)
    return { label: 'Not listening', tone: 'error' }
  return { label: 'Not reported', tone: 'idle' }
}

/** Down or degraded resolvers, worst first, for the page's verdict. */
export function resolverVerdict(
  resolvers: DnsResolverEntry[],
  enabled: boolean
): { tone: StatusTone; label: string } {
  if (!enabled) return { tone: 'idle', label: 'Cluster DNS is off' }
  const down = resolvers.filter((r) => r.health === 'down').length
  if (down > 0)
    return {
      tone: 'error',
      label: `${down} ${down === 1 ? 'resolver' : 'resolvers'} down`,
    }
  // Names resolve, but every HTTP call to them on that node fails.
  const routing = resolvers.filter(
    (r) =>
      r.internal_proxy?.listening === false &&
      (r.node_status === 'active' || r.node_status === 'draining')
  ).length
  if (routing > 0)
    return {
      tone: 'error',
      label: `Internal routing down on ${routing} ${routing === 1 ? 'node' : 'nodes'}`,
    }
  const degraded = resolvers.filter(
    (r) => HEALTH_META[r.health].tone === 'warn'
  ).length
  if (degraded > 0)
    return {
      tone: 'warn',
      label: `${degraded} ${degraded === 1 ? 'resolver needs' : 'resolvers need'} attention`,
    }
  return { tone: 'ok', label: 'All resolvers healthy' }
}

export const OUTCOME_LABEL: Record<DnsOutcome, string> = {
  noerror: 'NOERROR',
  nodata: 'NODATA',
  nxdomain: 'NXDOMAIN',
  servfail: 'SERVFAIL',
  refused: 'REFUSED',
  error: 'ERROR',
}

export function outcomeLabel(outcome: string): string {
  return (
    (OUTCOME_LABEL as Record<string, string>)[outcome] ?? outcome.toUpperCase()
  )
}

/** NXDOMAIN and NODATA are answers, not resolver failures. */
export function outcomeTone(outcome: string): StatusTone {
  if (outcome === 'noerror') return 'ok'
  if (outcome === 'nxdomain' || outcome === 'nodata') return 'warn'
  if (outcome === 'servfail' || outcome === 'refused' || outcome === 'error')
    return 'error'
  return 'idle'
}

/** `12 s`, `4 min`, `3 h`: how long ago a resolver last synced. */
export function formatAge(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined) return 'Never'
  if (seconds < 60) return `${seconds} s ago`
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min ago`
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} h ago`
  return `${Math.floor(seconds / 86_400)} d ago`
}

export function formatLatencyUs(us: number): string {
  if (us < 1000) return `${us} µs`
  return `${(us / 1000).toFixed(us < 10_000 ? 1 : 0)} ms`
}

/** Who asked: the attributed project/environment or database, else the IP. */
export function askedBy(entry: {
  project_name?: string | null
  environment_name?: string | null
  service_name?: string | null
  source_ip?: string
}): { primary: string; secondary?: string } {
  if (entry.service_name)
    return { primary: entry.service_name, secondary: 'database' }
  if (entry.project_name)
    return {
      primary: entry.project_name,
      secondary: entry.environment_name ?? undefined,
    }
  return { primary: entry.source_ip ?? 'Unknown' }
}

/** What owns a record, as a person would name it. */
export function recordOwner(record: DnsRecordEntry): string {
  if (record.service_name) return record.service_name
  if (record.project_name && record.environment_name)
    return `${record.project_name} / ${record.environment_name}`
  if (record.project_name) return record.project_name
  if (record.owner_kind === 'node') return record.node_name ?? 'Node'
  return record.owner_kind
}

export function recordTarget(record: DnsRecordEntry): string {
  if (!record.target_ip) return '—'
  return record.target_port
    ? `${record.target_ip}:${record.target_port}`
    : record.target_ip
}

// ── Traffic ─────────────────────────────────────────────────────────────────

export interface TrafficPoint {
  time: string
  queries: number
  nxdomain: number
  servfail: number
  /** NXDOMAIN share of internal queries, in percent; null with no queries. */
  nxdomain_rate: number | null
  latency_ms: number | null
}

function sum(counts: DnsTrafficBucket['internal']): number {
  return (
    counts.noerror +
    counts.nodata +
    counts.nxdomain +
    counts.servfail +
    counts.refused +
    counts.error
  )
}

/** Chart points for internal (`*.temps.local`) queries. */
export function trafficPoints(buckets: DnsTrafficBucket[]): TrafficPoint[] {
  return buckets.map((bucket) => {
    const queries = sum(bucket.internal)
    return {
      time: bucket.time,
      queries,
      nxdomain: bucket.internal.nxdomain,
      servfail:
        bucket.internal.servfail +
        bucket.internal.refused +
        bucket.internal.error,
      nxdomain_rate:
        queries > 0 ? (bucket.internal.nxdomain / queries) * 100 : null,
      latency_ms: bucket.internal_latency_ms ?? null,
    }
  })
}

export interface TrafficTotals {
  internal: number
  external: number
  failures: number
  nxdomain: number
  /** Failures (SERVFAIL, REFUSED, errors) as a share of internal queries. */
  failure_rate: number | null
  nxdomain_rate: number | null
}

export function trafficTotals(buckets: DnsTrafficBucket[]): TrafficTotals {
  let internal = 0
  let external = 0
  let failures = 0
  let nxdomain = 0
  for (const bucket of buckets) {
    internal += sum(bucket.internal)
    external += sum(bucket.external)
    failures +=
      bucket.internal.servfail + bucket.internal.refused + bucket.internal.error
    nxdomain += bucket.internal.nxdomain
  }
  return {
    internal,
    external,
    failures,
    nxdomain,
    failure_rate: internal > 0 ? (failures / internal) * 100 : null,
    nxdomain_rate: internal > 0 ? (nxdomain / internal) * 100 : null,
  }
}

export function formatPercent(value: number | null): string {
  if (value === null) return '—'
  if (value === 0) return '0%'
  return value < 1 ? `${value.toFixed(2)}%` : `${value.toFixed(1)}%`
}
