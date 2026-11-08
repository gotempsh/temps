// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  dnsKeys,
  type ClusterDnsResolversResponse,
  type DnsQueryLogResponse,
  type DnsRecordListResponse,
  type DnsTrafficResponse,
} from '@/lib/dns-observability'
import { DnsPage } from './DnsPage'

const resolvers: ClusterDnsResolversResponse = {
  cluster_dns_enabled: true,
  zone_record_count: 12,
  report_interval_secs: 30,
  resolvers: [
    {
      node_id: 0,
      node_name: 'control-plane',
      node_status: 'active',
      is_control_plane: true,
      health: 'healthy',
      running: true,
      tasks_alive: true,
      build_version: '0.9.0',
      listen_address: '172.18.0.1:53',
      record_count: 12,
      last_sync_age_secs: 2,
      consecutive_sync_failures: 0,
      dropped_log_entries: 0,
      reported_at: '2026-11-08T10:00:00Z',
    },
    {
      node_id: 7,
      node_name: 'worker-a',
      node_status: 'active',
      is_control_plane: false,
      health: 'down',
      health_reason: 'The resolver is not running: port 53 is in use',
      internal_proxy: {
        listening: false,
        address: '10.205.0.1:80',
        error: 'another program already listens on 10.205.0.1:80',
      },
      running: false,
      tasks_alive: false,
      consecutive_sync_failures: 3,
      dropped_log_entries: 0,
    },
  ],
  open_alerts: [
    {
      id: 41,
      alarm_type: 'dns_resolver_down',
      severity: 'critical',
      status: 'firing',
      title: 'DNS resolver down on worker-a',
      node_id: 7,
      fired_at: '2026-11-08T09:58:00Z',
    },
  ],
}

const queries: DnsQueryLogResponse = {
  items: [
    {
      id: 1,
      node_id: 0,
      node_name: 'control-plane',
      observed_at: '2026-11-08T10:00:01Z',
      qname: 'staging.shop.temps.local',
      qtype: 'A',
      zone: 'internal',
      outcome: 'nxdomain',
      latency_us: 180,
      answer_count: 0,
      source_ip: '172.18.0.9',
      project_id: 3,
      project_name: 'shop',
      environment_name: 'production',
    },
    {
      id: 2,
      node_id: 0,
      node_name: 'control-plane',
      observed_at: '2026-11-08T10:00:02Z',
      qtype: 'A',
      zone: 'external',
      outcome: 'servfail',
      latency_us: 3_000_000,
      answer_count: 0,
      source_ip: '172.18.0.9',
    },
  ],
  total: 2,
  page: 1,
  page_size: 20,
  retention_hours: 24,
}

const records: DnsRecordListResponse = {
  items: [
    {
      id: 5,
      fqdn: 'production.shop.temps.local',
      record_type: 'A',
      target_ip: '172.18.0.9',
      ttl: 5,
      owner_kind: 'environment',
      owner_id: 3,
      project_name: 'shop',
      environment_name: 'production',
      generation: 4,
      created_at: '2026-11-08T09:00:00Z',
      updated_at: '2026-11-08T09:30:00Z',
    },
  ],
  total: 1,
  page: 1,
  page_size: 20,
}

const traffic: DnsTrafficResponse = {
  node_id: 0,
  range: '1h',
  from: '2026-11-08T09:00:00Z',
  to: '2026-11-08T10:00:00Z',
  step_secs: 60,
  buckets: [
    {
      time: '2026-11-08T09:59:00Z',
      internal: {
        noerror: 18,
        nodata: 0,
        nxdomain: 2,
        servfail: 0,
        refused: 0,
        error: 0,
      },
      external: {
        noerror: 5,
        nodata: 0,
        nxdomain: 0,
        servfail: 1,
        refused: 0,
        error: 0,
      },
      internal_latency_ms: 0.2,
    },
  ],
}

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function seed(client: QueryClient) {
  client.setQueryData(dnsKeys.resolvers, resolvers)
  client.setQueryData(dnsKeys.queries({ page: 1, page_size: 20 }), queries)
  client.setQueryData(dnsKeys.records({ page: 1, page_size: 20 }), records)
  client.setQueryData(dnsKeys.traffic(0, '1h'), traffic)
}

function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  client
    .getQueryCache()
    .build(client, { queryKey })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}

function render(client: QueryClient) {
  const user = { id: 1, role: 'admin' } as unknown as UserResponse
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter>
          <BreadcrumbProvider>
            <DnsPage />
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

describe('DnsPage', () => {
  test('shows each resolver, the open alert and the verdict', () => {
    const client = createClient()
    seed(client)
    const html = render(client)
    expect(html).toContain('control-plane')
    expect(html).toContain('worker-a')
    expect(html).toContain('The resolver is not running: port 53 is in use')
    expect(html).toContain('1 resolver down')
    expect(html).toContain('DNS resolver down on worker-a')
    expect(html).toContain('href="/settings/notifications"')
    expect(html).toContain('href="/settings/nodes/7"')
    // The proxy that cannot listen is named with its address and reason.
    expect(html).toContain('Internal routing down on')
    expect(html).toContain('another program already listens on 10.205.0.1:80')
    expect(html).toContain('Main proxy')
    // The cluster-DNS switch is on this page too.
    expect(html).toContain('Cluster DNS')
    client.clear()
  })

  test('logs internal names and withholds forwarded ones', () => {
    const client = createClient()
    seed(client)
    const html = render(client)
    expect(html).toContain('staging.shop.temps.local')
    expect(html).toContain('NXDOMAIN')
    expect(html).toContain('SERVFAIL')
    expect(html).toContain('Forwarded name (not logged)')
    expect(html).toContain('shop')
    client.clear()
  })

  test('lists records with their owner and traffic totals', () => {
    const client = createClient()
    seed(client)
    const html = render(client)
    expect(html).toContain('production.shop.temps.local')
    expect(html).toContain('shop / production')
    expect(html).toContain('12 in the zone')
    expect(html).toContain('Internal queries')
    // 2 NXDOMAIN of 20 internal queries.
    expect(html).toContain('10.0%')
    expect(html).toContain('Test resolution')
    client.clear()
  })

  test('a refused read names the permission and offers no retry', () => {
    const client = createClient()
    fail(client, dnsKeys.resolvers, {
      title: 'Forbidden',
      status: 403,
      detail: 'Requires settings:read permission',
    })
    const html = render(client)
    expect(html).toContain('DNS resolvers: access denied')
    expect(html).not.toContain('Retry')
    client.clear()
  })

  test('a server error offers a retry', () => {
    const client = createClient()
    fail(client, dnsKeys.resolvers, {
      title: 'Internal Server Error',
      status: 500,
    })
    const html = render(client)
    expect(html).toContain('Retry')
    client.clear()
  })
})
