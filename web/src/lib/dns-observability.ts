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
  /** One of your sandboxes (ADR 105); never together with `project_id`. */
  sandbox_id: z.string().min(1).nullable(),
})
export type TestResolutionValues = z.infer<typeof testResolutionSchema>

/** `service_endpoints.owner_kind` values, with the label the filter shows. */
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

// ── Tabs and links ──────────────────────────────────────────────────────────

export const DNS_TABS = ['overview', 'records', 'queries', 'test'] as const
export type DnsTab = (typeof DNS_TABS)[number]

/** `?tab=`: Overview unless the value names another tab. */
export function parseDnsTab(value: string | null | undefined): DnsTab {
  return (DNS_TABS as readonly string[]).includes(value ?? '')
    ? (value as DnsTab)
    : 'overview'
}

/**
 * The page's query parameters. Each tab's filters carry its own prefix so
 * switching tabs keeps what the others were showing.
 */
export const DNS_PARAMS = {
  tab: 'tab',
  /** Overview: the node row opened (and scrolled to). */
  node: 'node',
  recordSearch: 'rq',
  recordOwner: 'rowner',
  recordNode: 'rnode',
  recordPage: 'rpage',
  recordPageSize: 'rsize',
  queryName: 'qq',
  queryNode: 'qnode',
  queryOutcome: 'qresult',
  queryZone: 'qzone',
  queryPage: 'qpage',
  queryPageSize: 'qsize',
  trafficNode: 'tnode',
  trafficRange: 'trange',
  testName: 'name',
  testType: 'type',
  testNode: 'from',
  testProject: 'project',
  testSandbox: 'sandbox',
} as const
export type DnsParam = (typeof DNS_PARAMS)[keyof typeof DNS_PARAMS]

/** A whole number from the URL, or `undefined` when absent or malformed. */
export function parseIntParam(value: string | null | undefined) {
  if (value === null || value === undefined || value.trim() === '')
    return undefined
  const number = Number(value)
  return Number.isInteger(number) ? number : undefined
}

/** The id of a node's row in Overview, which `?node=` opens. */
export function nodeRowId(nodeId: number) {
  return `dns-node-${nodeId}`
}

/** `?` + `current` with `patch` applied; `undefined` deletes a key. */
export function withParams(
  current: URLSearchParams,
  patch: Partial<Record<DnsParam, string | number | undefined>>
): string {
  const next = new URLSearchParams(current)
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined || value === '') next.delete(key)
    else next.set(key, String(value))
  }
  const search = next.toString()
  return search ? `?${search}` : '?'
}

export const DNS_PAGE_SIZES = [20, 50, 100] as const

/** `?rsize=` / `?qsize=`: one of the offered sizes, else 20. */
export function parsePageSize(value: string | null | undefined): number {
  const size = parseIntParam(value)
  return size !== undefined &&
    (DNS_PAGE_SIZES as readonly number[]).includes(size)
    ? size
    : DNS_PAGE_SIZES[0]
}

/** A node whose resolver can answer: not off, offline or absent. */
function answers(resolver: DnsResolverEntry): boolean {
  return (
    resolver.health !== 'no_local_resolver' &&
    resolver.health !== 'off' &&
    resolver.health !== 'node_unavailable'
  )
}

/**
 * The node to chart or test from when none is chosen: `preferred` when its
 * resolver can answer, else the first that can (a hosted control plane runs
 * none), else the control plane.
 */
export function defaultResolverNode(
  resolvers: DnsResolverEntry[],
  preferred?: number | null
): number {
  const usable = resolvers.filter(answers)
  if (
    preferred !== undefined &&
    preferred !== null &&
    usable.some((r) => r.node_id === preferred)
  )
    return preferred
  return usable[0]?.node_id ?? resolvers[0]?.node_id ?? CONTROL_PLANE_NODE_ID
}

/**
 * The Test tab's fields for a record: its name and type, asked from the
 * node that serves it (a record with no node is the control plane's).
 */
export function recordTestParams(
  record: DnsRecordEntry,
  resolvers: DnsResolverEntry[]
): { name: string; type: DnsTestRecordType; from: number } {
  const type = (DNS_TEST_RECORD_TYPES as readonly string[]).includes(
    record.record_type
  )
    ? (record.record_type as DnsTestRecordType)
    : 'A'
  return {
    name: record.fqdn,
    type,
    from: defaultResolverNode(
      resolvers,
      record.node_id ?? CONTROL_PLANE_NODE_ID
    ),
  }
}

/**
 * The form's values from the URL (`name`, `type`, `from`, `project`,
 * `sandbox`). A link naming both a project and a sandbox asks as the
 * sandbox: the server refuses both at once.
 */
export function testValuesFromUrl(
  get: (key: DnsParam) => string | null,
  resolvers: DnsResolverEntry[]
): TestResolutionValues {
  const type = get(DNS_PARAMS.testType)
  const from = parseIntParam(get(DNS_PARAMS.testNode))
  const project = parseIntParam(get(DNS_PARAMS.testProject))
  const sandbox = get(DNS_PARAMS.testSandbox)?.trim() || null
  return {
    name: get(DNS_PARAMS.testName) ?? '',
    record_type: (DNS_TEST_RECORD_TYPES as readonly string[]).includes(
      type ?? ''
    )
      ? (type as DnsTestRecordType)
      : 'A',
    // A node named in the link is kept as asked, even if it cannot answer:
    // the error then says why.
    node_id:
      from !== undefined && resolvers.some((r) => r.node_id === from)
        ? from
        : defaultResolverNode(resolvers),
    project_id:
      sandbox === null && project !== undefined && project > 0 ? project : null,
    sandbox_id: sandbox,
  }
}

// ── Health ──────────────────────────────────────────────────────────────────

/** One half of a node's internal DNS: name lookups, or internal routing. */
export interface DnsFacet {
  tone: StatusTone
  label: string
  /** What is wrong, in the API's words; `null` while it works. */
  reason: string | null
  /** The longer explanation behind "Why?". */
  why: string | null
}

export type DnsFacetArea = 'lookups' | 'routing'

export interface NodeDnsHealth {
  nodeId: number
  nodeName: string
  /** The worst of name lookups and internal routing. */
  tone: StatusTone
  label: string
  /**
   * One calm line instead of the two facets, for a node with nothing to
   * check: cluster DNS off, the node unavailable, or a control plane that
   * runs no apps.
   */
  note: string | null
  /** Behind "Why?" next to `note`. */
  noteWhy: string | null
  lookups: DnsFacet
  /** `null` on the control plane: its main proxy routes internal names. */
  routing: DnsFacet | null
  /** The facet to lead with: the worse one that is not working. */
  problem: (DnsFacet & { area: DnsFacetArea }) | null
}

const TONE_RANK: Record<StatusTone, number> = {
  error: 3,
  warn: 2,
  running: 1,
  ok: 1,
  idle: 0,
}

/** Name lookups on a node: its resolver, as the server judged it. */
function lookupsFacet(resolver: DnsResolverEntry): DnsFacet {
  const reason = resolver.health_reason ?? null
  const node = resolver.node_name
  switch (resolver.health) {
    case 'healthy':
      return { tone: 'ok', label: 'Working', reason: null, why: null }
    case 'stale':
      return {
        tone: 'warn',
        label: 'Out of date',
        reason: reason ?? 'The records have not synced recently',
        why: `${node} answers from an old copy of the records, so a service that is new or has moved may not resolve there.`,
      }
    case 'down':
      return {
        tone: 'error',
        label: 'Down',
        reason: reason ?? 'The resolver is not running',
        why: `Apps on ${node} cannot look up *.temps.local names until its resolver runs again.`,
      }
    case 'agent_too_old':
      return {
        tone: 'warn',
        label: 'Agent too old',
        reason,
        why: 'Lookups may still work, but this agent cannot report its queries and traffic.',
      }
    case 'not_reporting':
      return {
        tone: 'warn',
        label: 'Not reporting',
        reason,
        why: `Temps cannot tell whether lookups work on ${node} until its agent reports again.`,
      }
    case 'off':
      return { tone: 'idle', label: 'Off', reason: null, why: null }
    case 'node_unavailable':
      return { tone: 'idle', label: 'Node unavailable', reason, why: null }
    case 'no_local_resolver':
      return { tone: 'idle', label: 'Not needed', reason: null, why: reason }
  }
}

/**
 * Served, but through the agent's fallback on port 80 because the port-80
 * redirect to the internal proxy is missing (ADR-098): the agent holds port
 * 80 of the bridge address, and `redirect_error` says why.
 */
export function redirectMissing(
  proxy: InternalProxyState | null | undefined
): boolean {
  return (
    proxy?.listening === true &&
    proxy.mode === 'direct' &&
    proxy.redirect === 'missing'
  )
}

/**
 * Where a node's internal routing listens: a worker's bridge address or the
 * control plane's app network gateway (ADR-104), and where a port-80
 * redirect sends it (ADR-098).
 */
export function routingAddress(
  proxy: InternalProxyState | null | undefined
): string | null {
  if (!proxy?.address) return null
  return proxy.mode === 'redirect' && proxy.proxy_address
    ? `${proxy.address} → ${proxy.proxy_address}`
    : proxy.address
}

/**
 * Where a node's `*.temps.local` names point: a worker's bridge address, or,
 * with no worker nodes, the control plane's app network gateway (ADR-104).
 */
function servedOn(resolver: DnsResolverEntry): string {
  return resolver.is_control_plane
    ? 'its app network gateway'
    : 'its bridge address'
}

/** Who serves the calls: a worker's agent, or the control plane itself. */
function server(resolver: DnsResolverEntry): string {
  return resolver.is_control_plane ? 'the control plane' : 'its agent'
}

/**
 * Internal routing: the proxy behind a node's `*.temps.local` names. `null`
 * for a control plane that does not report one: its main proxy routes.
 */
function routingFacet(resolver: DnsResolverEntry): DnsFacet | null {
  const proxy = resolver.internal_proxy
  if (!proxy) return null
  // Nothing routes through an offline node, so its proxy is no outage.
  if (resolver.node_status !== 'active' && resolver.node_status !== 'draining')
    return { tone: 'idle', label: 'Not checked', reason: null, why: null }
  if (redirectMissing(proxy)) {
    const address = proxy.address ?? servedOn(resolver)
    return {
      tone: 'warn',
      label: 'Direct on port 80',
      reason: proxy.redirect_error ?? 'No reason reported',
      why: `Calls to *.temps.local names work, but ${resolver.node_name} redirects nothing: ${server(resolver)} serves them on ${address} itself, so it holds port 80 of ${servedOn(resolver)}. Once the redirect can be installed, it moves to its own port and releases port 80.`,
    }
  }
  if (proxy.listening === true && proxy.mode === 'redirect')
    return { tone: 'ok', label: 'Redirected', reason: null, why: null }
  // An agent older than ADR-098, or one that cannot redirect (not Linux).
  if (proxy.listening === true)
    return { tone: 'ok', label: 'Working', reason: null, why: null }
  if (proxy.listening === false) {
    const address = proxy.address ?? servedOn(resolver)
    return {
      tone: 'error',
      label: 'Down',
      reason: proxy.error ?? 'No reason reported',
      why: resolver.is_control_plane
        ? `The control plane's containers cannot call *.temps.local names: their names are withdrawn until its internal routing proxy can listen on ${address}. The control plane keeps retrying, so this clears by itself once the cause is gone.`
        : `Names resolve on ${resolver.node_name}, but every HTTP call to them fails until its internal routing proxy can listen on ${address}. The agent keeps retrying, so this clears by itself once the cause is gone.`,
    }
  }
  return {
    tone: 'idle',
    label: 'Not reported',
    reason: null,
    why: resolver.is_control_plane
      ? 'The control plane has not reported its internal routing yet.'
      : 'Older agents, and a node with no overlay network yet, do not report internal routing.',
  }
}

const COMBINED_LABEL: Record<StatusTone, string> = {
  error: 'Not working',
  warn: 'Needs attention',
  ok: 'Working',
  running: 'Working',
  idle: 'Unknown',
}

/**
 * One health per node: the worst of its name lookups and internal routing,
 * so a resolver that answers never shows "Working" beside routing that is
 * down.
 */
export function nodeDnsHealth(
  resolver: DnsResolverEntry,
  enabled: boolean
): NodeDnsHealth {
  const lookups = lookupsFacet(resolver)
  const base = {
    nodeId: resolver.node_id,
    nodeName: resolver.node_name,
    lookups,
    problem: null,
    noteWhy: null,
  }
  if (!enabled || resolver.health === 'off')
    return {
      ...base,
      tone: 'idle',
      label: 'Off',
      note: 'Cluster DNS is off',
      routing: null,
    }
  if (resolver.health === 'no_local_resolver')
    return {
      ...base,
      tone: 'idle',
      label: 'Not needed',
      note: 'This control plane runs no apps',
      noteWhy: lookups.why,
      routing: null,
    }
  if (resolver.health === 'node_unavailable')
    return {
      ...base,
      tone: 'idle',
      label: 'Unavailable',
      note: resolver.health_reason ?? 'The node is not active',
      routing: null,
    }

  const routing = routingFacet(resolver)
  const facets: (DnsFacet & { area: DnsFacetArea })[] = [
    { ...lookups, area: 'lookups' },
    ...(routing ? [{ ...routing, area: 'routing' as const }] : []),
  ]
  // Lookups first on a tie: when they fail, nothing else matters.
  const worst = facets.reduce((a, b) =>
    TONE_RANK[b.tone] > TONE_RANK[a.tone] ? b : a
  )
  const tone = TONE_RANK[worst.tone] >= TONE_RANK.ok ? worst.tone : 'idle'
  const problem = worst.tone === 'error' || worst.tone === 'warn' ? worst : null
  return {
    ...base,
    tone,
    label: COMBINED_LABEL[tone],
    note: null,
    routing,
    problem,
  }
}

export interface DnsSummary {
  tone: StatusTone
  /** The header badge, e.g. "Internal routing down on 1 node". */
  label: string
  /** One plain sentence: what it means for apps. */
  detail: string
  /** The nodes the badge points at, worst first. */
  affected: NodeDnsHealth[]
  nodes: NodeDnsHealth[]
}

function nodes(count: number) {
  return `${count} ${count === 1 ? 'node' : 'nodes'}`
}

function names(list: NodeDnsHealth[]) {
  const shown = list.slice(0, 3).map((n) => n.nodeName)
  const more = list.length - shown.length
  return more > 0 ? `${shown.join(', ')} and ${more} more` : shown.join(', ')
}

/** Is internal DNS working, across every node, in one line. */
export function dnsSummary(
  resolvers: DnsResolverEntry[],
  enabled: boolean
): DnsSummary {
  const all = resolvers.map((r) => nodeDnsHealth(r, enabled))
  if (!enabled)
    return {
      tone: 'idle',
      label: 'Cluster DNS is off',
      detail:
        'Apps cannot reach each other by *.temps.local name. Turn on cluster DNS below.',
      affected: [],
      nodes: all,
    }
  const failing = all.filter((n) => n.tone === 'error')
  if (failing.length > 0) {
    const routingOnly = failing.every((n) => n.lookups.tone !== 'error')
    const lookupsOnly = failing.every(
      (n) => n.lookups.tone === 'error' && n.routing?.tone !== 'error'
    )
    const label = routingOnly
      ? `Internal routing down on ${nodes(failing.length)}`
      : lookupsOnly
        ? `Name lookups down on ${nodes(failing.length)}`
        : `Internal DNS not working on ${nodes(failing.length)}`
    const detail = routingOnly
      ? `Names resolve, but HTTP calls to *.temps.local names fail on ${names(failing)}.`
      : `Apps on ${names(failing)} cannot reach other services by name.`
    return { tone: 'error', label, detail, affected: failing, nodes: all }
  }
  const attention = all.filter((n) => n.tone === 'warn')
  // Served, but the agent holds port 80 of the bridge address (ADR-098).
  const directOnly = attention.every(
    (n) => n.lookups.tone !== 'warn' && n.routing?.label === 'Direct on port 80'
  )
  if (attention.length > 0 && directOnly)
    return {
      tone: 'warn',
      label: `Port-80 redirect missing on ${nodes(attention.length)}`,
      detail: `Lookups and calls work, but the agent on ${names(attention)} holds port 80 of its bridge address.`,
      affected: attention,
      nodes: all,
    }
  if (attention.length > 0)
    return {
      tone: 'warn',
      label: `${nodes(attention.length)} ${attention.length === 1 ? 'needs' : 'need'} attention`,
      detail: `Lookups work, but check ${names(attention)}.`,
      affected: attention,
      nodes: all,
    }
  const considered = all.filter((n) => n.label !== 'Not needed')
  const working = considered.filter((n) => n.tone === 'ok')
  if (working.length === 0)
    return {
      tone: 'idle',
      label: 'No node runs apps yet',
      detail: 'A resolver starts on each worker that runs apps.',
      affected: [],
      nodes: all,
    }
  return {
    tone: 'ok',
    label:
      working.length === considered.length
        ? 'Working on all nodes'
        : `Working on ${working.length} of ${considered.length} nodes`,
    detail: 'Apps can reach each other by *.temps.local name.',
    affected: [],
    nodes: all,
  }
}

export const OUTCOME_LABEL: Record<DnsOutcome, string> = {
  noerror: 'NOERROR',
  nodata: 'NODATA',
  nxdomain: 'NXDOMAIN',
  servfail: 'SERVFAIL',
  refused: 'REFUSED',
  error: 'ERROR',
}

/** What an answer means, in plain words, for the Test tab's result. */
export function outcomeMeaning(outcome: string, recordType: string): string {
  switch (outcome) {
    case 'noerror':
      return 'Found'
    case 'nxdomain':
      return 'No such name'
    case 'nodata':
      return `The name exists, but has no ${recordType} record`
    case 'servfail':
      return 'The resolver failed to answer'
    case 'refused':
      return 'The resolver refused to answer'
    default:
      return 'The lookup failed'
  }
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
