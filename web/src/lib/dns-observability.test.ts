// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  askedBy,
  formatAge,
  formatLatencyUs,
  formatPercent,
  defaultResolverNode,
  dnsSummary,
  nodeDnsHealth,
  outcomeLabel,
  outcomeMeaning,
  outcomeTone,
  recordOwner,
  recordTarget,
  parseDnsTab,
  parseIntParam,
  parsePageSize,
  recordTestParams,
  redirectMissing,
  routingAddress,
  testValuesFromUrl,
  withParams,
  trafficPoints,
  trafficTotals,
  type DnsRecordEntry,
  type DnsResolverEntry,
  type DnsTrafficBucket,
} from './dns-observability'

function resolver(overrides: Partial<DnsResolverEntry>): DnsResolverEntry {
  return {
    node_id: 0,
    node_name: 'control-plane',
    node_status: 'active',
    is_control_plane: true,
    health: 'healthy',
    running: true,
    tasks_alive: true,
    consecutive_sync_failures: 0,
    dropped_log_entries: 0,
    ...overrides,
  }
}

function counts(overrides: Partial<DnsTrafficBucket['internal']> = {}) {
  return {
    noerror: 0,
    nodata: 0,
    nxdomain: 0,
    servfail: 0,
    refused: 0,
    error: 0,
    ...overrides,
  }
}

function record(overrides: Partial<DnsRecordEntry>): DnsRecordEntry {
  return {
    id: 1,
    fqdn: 'production.shop.temps.local',
    record_type: 'A',
    target_ip: '10.0.0.5',
    ttl: 5,
    owner_kind: 'environment',
    owner_id: 3,
    generation: 1,
    created_at: '2026-11-01T00:00:00Z',
    updated_at: '2026-11-01T00:00:00Z',
    ...overrides,
  }
}

/**
 * A worker whose resolver answers but whose internal routing proxy cannot
 * bind its address: the case that showed a green "Healthy" beside a red
 * error. Test data only; the reason is whatever the agent reported.
 */
const master = resolver({
  node_id: 4,
  node_name: 'master',
  is_control_plane: false,
  listen_address: '10.205.0.1:53',
  internal_proxy: {
    listening: false,
    address: '10.205.0.1:80',
    error: 'another program already listens on 10.205.0.1:80',
  },
})

/** A hosted control plane: no containers run on it, so no resolver. */
const hostedControlPlane = resolver({
  health: 'no_local_resolver',
  health_reason:
    'The control plane runs no resolver of its own: no containers run on it, so none was started.',
})

describe('nodeDnsHealth', () => {
  test('working lookups and down routing make one "Not working" health', () => {
    const health = nodeDnsHealth(master, true)
    expect(health.tone).toBe('error')
    expect(health.label).toBe('Not working')
    expect(health.lookups).toMatchObject({ tone: 'ok', label: 'Working' })
    expect(health.routing).toMatchObject({ tone: 'error', label: 'Down' })
    // The reason is the agent's own text, never advice the page made up.
    expect(health.problem).toMatchObject({
      area: 'routing',
      reason: 'another program already listens on 10.205.0.1:80',
    })
    expect(health.problem?.why).toContain('10.205.0.1:80')
    expect(health.note).toBeNull()
  })

  test('a control plane that runs no apps is calm, never a warning', () => {
    const health = nodeDnsHealth(hostedControlPlane, true)
    expect(health.tone).toBe('idle')
    expect(health.label).toBe('Not needed')
    expect(health.note).toBe('This control plane runs no apps')
    expect(health.noteWhy).toContain('no containers run on it')
    expect(health.problem).toBeNull()
  })

  test('lookups down outrank routing down, and lead', () => {
    const health = nodeDnsHealth(
      resolver({
        ...master,
        health: 'down',
        health_reason: 'The resolver is not running',
      }),
      true
    )
    expect(health.tone).toBe('error')
    expect(health.problem).toMatchObject({
      area: 'lookups',
      reason: 'The resolver is not running',
    })
  })

  test('stale, too old and silent resolvers need attention', () => {
    for (const health of ['stale', 'agent_too_old', 'not_reporting'] as const)
      expect(nodeDnsHealth(resolver({ health }), true)).toMatchObject({
        tone: 'warn',
        label: 'Needs attention',
      })
  })

  test('the control plane routes with its main proxy', () => {
    const health = nodeDnsHealth(resolver({}), true)
    expect(health).toMatchObject({ tone: 'ok', label: 'Working' })
    expect(health.routing).toBeNull()
  })

  test('an unreported proxy is unknown, not down', () => {
    const health = nodeDnsHealth(
      resolver({ node_id: 5, is_control_plane: false, internal_proxy: {} }),
      true
    )
    expect(health.tone).toBe('ok')
    expect(health.routing).toMatchObject({
      tone: 'idle',
      label: 'Not reported',
    })
  })

  test('off and unavailable nodes are a note, not a problem', () => {
    expect(nodeDnsHealth(master, false)).toMatchObject({
      tone: 'idle',
      label: 'Off',
      problem: null,
    })
    expect(
      nodeDnsHealth(
        resolver({
          ...master,
          node_status: 'offline',
          health: 'node_unavailable',
          health_reason: 'The node is offline',
        }),
        true
      )
    ).toMatchObject({
      tone: 'idle',
      label: 'Unavailable',
      note: 'The node is offline',
      problem: null,
    })
  })
})

describe('internal routing with its own port (ADR-098)', () => {
  /** A worker whose agent could not install the port-80 redirect. */
  const fallback = {
    listening: true,
    address: '10.205.0.1:80',
    mode: 'direct' as const,
    redirect: 'missing' as const,
    redirect_error: 'nft (nftables) is not installed on this worker',
  }
  const worker = (internal_proxy: DnsResolverEntry['internal_proxy']) =>
    resolver({
      node_id: 4,
      node_name: 'master',
      is_control_plane: false,
      internal_proxy,
    })

  test('labels: redirected, direct on port 80, working, down, not reported', () => {
    expect(
      nodeDnsHealth(
        worker({
          listening: true,
          address: '10.205.0.1:80',
          mode: 'redirect',
          redirect: 'installed',
          proxy_address: '10.205.0.1:591',
        }),
        true
      ).routing
    ).toMatchObject({ tone: 'ok', label: 'Redirected', reason: null })
    expect(nodeDnsHealth(worker(fallback), true).routing).toMatchObject({
      tone: 'warn',
      label: 'Direct on port 80',
      // The agent's own reason, never advice the page made up.
      reason: 'nft (nftables) is not installed on this worker',
    })
    // Not Linux: direct is all there is, nothing to fix.
    expect(
      nodeDnsHealth(worker({ ...fallback, redirect: 'unsupported' }), true)
        .routing
    ).toMatchObject({ tone: 'ok', label: 'Working' })
    // An agent from before ADR-098.
    expect(
      nodeDnsHealth(worker({ listening: true }), true).routing
    ).toMatchObject({ tone: 'ok', label: 'Working' })
    expect(
      nodeDnsHealth(worker({ listening: false, mode: 'down' }), true).routing
    ).toMatchObject({ tone: 'error', label: 'Down' })
    expect(nodeDnsHealth(worker({}), true).routing?.label).toBe('Not reported')
  })

  test('redirectMissing is only the served fallback, not down or unsupported', () => {
    expect(redirectMissing(fallback)).toBe(true)
    expect(redirectMissing({ ...fallback, redirect: 'unsupported' })).toBe(
      false
    )
    expect(redirectMissing({ ...fallback, listening: false })).toBe(false)
    expect(redirectMissing(null)).toBe(false)
  })

  test('a missing redirect warns but does not fail the summary', () => {
    const health = nodeDnsHealth(worker(fallback), true)
    expect(health.tone).toBe('warn')
    expect(health.problem).toMatchObject({ area: 'routing' })
    expect(health.problem?.why).toContain('10.205.0.1:80')
    expect(dnsSummary([resolver({}), worker(fallback)], true)).toMatchObject({
      tone: 'warn',
      label: 'Port-80 redirect missing on 1 node',
    })
    // A node that is down wins over the warning.
    expect(
      dnsSummary(
        [
          worker(fallback),
          resolver({
            node_id: 5,
            is_control_plane: false,
            internal_proxy: { listening: false },
          }),
        ],
        true
      ).tone
    ).toBe('error')
  })

  test('the address shows where a redirect sends port 80', () => {
    expect(
      routingAddress({
        address: '10.205.0.1:80',
        mode: 'redirect',
        proxy_address: '10.205.0.1:591',
      })
    ).toBe('10.205.0.1:80 → 10.205.0.1:591')
    expect(routingAddress(fallback)).toBe('10.205.0.1:80')
    expect(routingAddress(null)).toBeNull()
  })
})

describe('dnsSummary', () => {
  test('says Cluster DNS is off before judging nodes', () => {
    expect(dnsSummary([resolver({ health: 'down' })], false)).toMatchObject({
      tone: 'idle',
      label: 'Cluster DNS is off',
      affected: [],
    })
  })

  test('routing down on master: the badge names it and points at the row', () => {
    const summary = dnsSummary([hostedControlPlane, master], true)
    expect(summary.tone).toBe('error')
    expect(summary.label).toBe('Internal routing down on 1 node')
    expect(summary.detail).toContain('master')
    expect(summary.affected.map((node) => node.nodeId)).toEqual([4])
    // The hosted control plane is not among the problems.
    expect(summary.nodes[0].label).toBe('Not needed')
  })

  test('lookups down, and both kinds at once', () => {
    const down = resolver({ node_id: 2, health: 'down' })
    expect(
      dnsSummary([down, resolver({ node_id: 3, health: 'down' })], true)
    ).toMatchObject({ label: 'Name lookups down on 2 nodes' })
    expect(dnsSummary([down, master], true)).toMatchObject({
      label: 'Internal DNS not working on 2 nodes',
    })
  })

  test('degraded nodes need attention', () => {
    expect(
      dnsSummary(
        [
          resolver({ health: 'stale' }),
          resolver({ node_id: 2, health: 'not_reporting' }),
        ],
        true
      )
    ).toMatchObject({ tone: 'warn', label: '2 nodes need attention' })
  })

  test('a hosted control plane and healthy workers are working everywhere', () => {
    expect(
      dnsSummary(
        [hostedControlPlane, resolver({ node_id: 1, is_control_plane: false })],
        true
      )
    ).toMatchObject({ tone: 'ok', label: 'Working on all nodes' })
  })

  test('a hosted control plane with no workers yet is not an alarm', () => {
    expect(dnsSummary([hostedControlPlane], true)).toMatchObject({
      tone: 'idle',
      label: 'No node runs apps yet',
    })
  })

  test('an unavailable node is counted, not alarmed', () => {
    expect(
      dnsSummary(
        [resolver({}), resolver({ node_id: 4, health: 'node_unavailable' })],
        true
      )
    ).toMatchObject({ tone: 'ok', label: 'Working on 1 of 2 nodes' })
  })
})

describe('tabs and links', () => {
  test('the tab comes from ?tab=, Overview by default', () => {
    expect(parseDnsTab(null)).toBe('overview')
    expect(parseDnsTab('records')).toBe('records')
    expect(parseDnsTab('queries')).toBe('queries')
    expect(parseDnsTab('test')).toBe('test')
    expect(parseDnsTab('traffic')).toBe('overview')
  })

  test('withParams keeps other params and deletes undefined ones', () => {
    const current = new URLSearchParams('tab=records&rq=shop&node=4')
    expect(withParams(current, { tab: undefined, node: 7 })).toBe(
      '?rq=shop&node=7'
    )
    expect(withParams(new URLSearchParams(), { tab: undefined })).toBe('?')
  })

  test('page sizes and numbers from the URL fall back safely', () => {
    expect(parsePageSize('50')).toBe(50)
    expect(parsePageSize('37')).toBe(20)
    expect(parsePageSize(null)).toBe(20)
    expect(parseIntParam('7')).toBe(7)
    expect(parseIntParam('x')).toBeUndefined()
    expect(parseIntParam('')).toBeUndefined()
  })

  test('a record is tested from the node that serves it', () => {
    const resolvers = [hostedControlPlane, master]
    expect(
      recordTestParams(
        record({ fqdn: 'primary.pg-orders.temps.local', node_id: 4 }),
        resolvers
      )
    ).toEqual({ name: 'primary.pg-orders.temps.local', type: 'A', from: 4 })
  })

  test('a control-plane record on a hosted plane is tested from a worker', () => {
    // The control plane runs no resolver, so asking it would only fail.
    expect(
      recordTestParams(record({ record_type: 'SRV' }), [
        hostedControlPlane,
        master,
      ])
    ).toEqual({ name: 'production.shop.temps.local', type: 'SRV', from: 4 })
  })

  test('a record type the test cannot ask falls back to A', () => {
    expect(
      recordTestParams(record({ record_type: 'PTR' }), [resolver({})]).type
    ).toBe('A')
  })

  test('the Test form reads its fields from the URL', () => {
    const params = new URLSearchParams(
      'name=production.shop.temps.local&type=AAAA&from=4&project=3'
    )
    const get = (key: string) => params.get(key)
    expect(testValuesFromUrl(get, [hostedControlPlane, master])).toEqual({
      name: 'production.shop.temps.local',
      record_type: 'AAAA',
      node_id: 4,
      project_id: 3,
    })
    // Nothing in the URL: an empty name, A, the first node that answers.
    expect(testValuesFromUrl(() => null, [hostedControlPlane, master])).toEqual(
      { name: '', record_type: 'A', node_id: 4, project_id: null }
    )
    // An unknown node or type is not trusted.
    params.set('from', '99')
    params.set('type', 'MX')
    expect(testValuesFromUrl(get, [resolver({})])).toMatchObject({
      record_type: 'A',
      node_id: 0,
    })
  })

  test('the default node skips resolvers that cannot answer', () => {
    expect(defaultResolverNode([hostedControlPlane, master])).toBe(4)
    expect(defaultResolverNode([resolver({}), master], 4)).toBe(4)
    expect(defaultResolverNode([resolver({}), master], 99)).toBe(0)
    expect(defaultResolverNode([hostedControlPlane])).toBe(0)
  })
})

describe('outcomeMeaning', () => {
  test('says what an answer means in plain words', () => {
    expect(outcomeMeaning('noerror', 'A')).toBe('Found')
    expect(outcomeMeaning('nxdomain', 'A')).toBe('No such name')
    expect(outcomeMeaning('nodata', 'AAAA')).toBe(
      'The name exists, but has no AAAA record'
    )
    expect(outcomeMeaning('servfail', 'A')).toBe(
      'The resolver failed to answer'
    )
  })
})

describe('outcomes', () => {
  test('NXDOMAIN and NODATA are answers, SERVFAIL is a failure', () => {
    expect(outcomeTone('noerror')).toBe('ok')
    expect(outcomeTone('nxdomain')).toBe('warn')
    expect(outcomeTone('nodata')).toBe('warn')
    expect(outcomeTone('servfail')).toBe('error')
    expect(outcomeTone('refused')).toBe('error')
    expect(outcomeTone('something-new')).toBe('idle')
  })

  test('labels are the DNS response codes', () => {
    expect(outcomeLabel('nxdomain')).toBe('NXDOMAIN')
    expect(outcomeLabel('servfail')).toBe('SERVFAIL')
    expect(outcomeLabel('badcookie')).toBe('BADCOOKIE')
  })
})

describe('formatting', () => {
  test('sync age', () => {
    expect(formatAge(null)).toBe('Never')
    expect(formatAge(undefined)).toBe('Never')
    expect(formatAge(0)).toBe('0 s ago')
    expect(formatAge(59)).toBe('59 s ago')
    expect(formatAge(60)).toBe('1 min ago')
    expect(formatAge(7200)).toBe('2 h ago')
    expect(formatAge(172_800)).toBe('2 d ago')
  })

  test('latency', () => {
    expect(formatLatencyUs(250)).toBe('250 µs')
    expect(formatLatencyUs(1500)).toBe('1.5 ms')
    expect(formatLatencyUs(42_000)).toBe('42 ms')
  })

  test('percent', () => {
    expect(formatPercent(null)).toBe('—')
    expect(formatPercent(0)).toBe('0%')
    expect(formatPercent(0.25)).toBe('0.25%')
    expect(formatPercent(12.345)).toBe('12.3%')
  })
})

describe('askedBy', () => {
  test('a database is named before a project', () => {
    expect(
      askedBy({
        service_name: 'orders-db',
        project_name: 'shop',
        source_ip: '10.0.0.9',
      })
    ).toEqual({ primary: 'orders-db', secondary: 'database' })
  })

  test('a project with its environment', () => {
    expect(
      askedBy({
        project_name: 'shop',
        environment_name: 'production',
        source_ip: '10.0.0.9',
      })
    ).toEqual({ primary: 'shop', secondary: 'production' })
  })

  test('an unattributed container is its address', () => {
    expect(askedBy({ source_ip: '10.0.0.9' })).toEqual({ primary: '10.0.0.9' })
  })
})

describe('records', () => {
  test('owner names the database, the environment or the node', () => {
    expect(recordOwner(record({ service_name: 'orders-db' }))).toBe('orders-db')
    expect(
      recordOwner(
        record({ project_name: 'shop', environment_name: 'production' })
      )
    ).toBe('shop / production')
    expect(
      recordOwner(record({ owner_kind: 'node', node_name: 'worker-1' }))
    ).toBe('worker-1')
    expect(recordOwner(record({ owner_kind: 'static' }))).toBe('static')
  })

  test('target carries the port when the record has one', () => {
    expect(recordTarget(record({}))).toBe('10.0.0.5')
    expect(recordTarget(record({ target_port: 5432 }))).toBe('10.0.0.5:5432')
    expect(recordTarget(record({ target_ip: null }))).toBe('—')
  })
})

describe('traffic', () => {
  const buckets: DnsTrafficBucket[] = [
    {
      time: '2026-11-08T10:00:00Z',
      internal: counts({ noerror: 90, nxdomain: 8, servfail: 1, error: 1 }),
      external: counts({ noerror: 40 }),
      internal_latency_ms: 0.4,
    },
    {
      time: '2026-11-08T10:01:00Z',
      internal: counts(),
      external: counts({ servfail: 2 }),
      internal_latency_ms: null,
    },
  ]

  test('points chart internal queries and leave the rate empty with none', () => {
    const points = trafficPoints(buckets)
    expect(points[0]).toEqual({
      time: '2026-11-08T10:00:00Z',
      queries: 100,
      nxdomain: 8,
      servfail: 2,
      nxdomain_rate: 8,
      latency_ms: 0.4,
    })
    expect(points[1].queries).toBe(0)
    expect(points[1].nxdomain_rate).toBeNull()
    expect(points[1].latency_ms).toBeNull()
  })

  test('totals count forwarded queries apart from internal failures', () => {
    expect(trafficTotals(buckets)).toEqual({
      internal: 100,
      external: 42,
      failures: 2,
      nxdomain: 8,
      failure_rate: 2,
      nxdomain_rate: 8,
    })
    expect(trafficTotals([]).failure_rate).toBeNull()
  })
})
