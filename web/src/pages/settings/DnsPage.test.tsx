// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render as renderDom,
  waitFor,
} from '@testing-library/react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
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
        error:
          "containers' calls to port 80 of 10.205.0.1 are not redirected to the internal proxy on 10.205.0.1:591: nft (nftables) is not installed on this worker",
        mode: 'down',
        redirect: 'missing',
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

function Page({
  client,
  url = '/settings/dns',
}: {
  client: QueryClient
  url?: string
}) {
  const user = { id: 1, role: 'admin' } as unknown as UserResponse
  return (
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
        <MemoryRouter initialEntries={[url]}>
          <BreadcrumbProvider>
            <DnsPage />
            <Search />
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

/** The current query string, so a test can assert what the URL holds. */
function Search() {
  const { search } = useLocation()
  return <output data-testid="search">{search}</output>
}

function render(client: QueryClient, url?: string) {
  return renderToStaticMarkup(<Page client={client} url={url} />)
}

/**
 * The reported shape: the resolver answers, but the internal routing proxy
 * cannot bind its address (here because another program holds it), beside
 * a hosted control plane that runs no apps.
 */
const masterCase: ClusterDnsResolversResponse = {
  cluster_dns_enabled: true,
  zone_record_count: 3,
  report_interval_secs: 30,
  open_alerts: [],
  resolvers: [
    {
      node_id: 0,
      node_name: 'control-plane',
      node_status: 'active',
      is_control_plane: true,
      health: 'no_local_resolver',
      health_reason:
        'The control plane runs no resolver of its own: no containers run on it, so none was started.',
      running: false,
      tasks_alive: false,
      consecutive_sync_failures: 0,
      dropped_log_entries: 0,
    },
    {
      node_id: 4,
      node_name: 'master',
      node_status: 'active',
      is_control_plane: false,
      health: 'healthy',
      listen_address: '10.205.0.1:53',
      build_version: '0.9.1',
      record_count: 3,
      last_sync_age_secs: 4,
      reported_at: '2026-11-08T10:00:00Z',
      internal_proxy: {
        listening: false,
        address: '10.205.0.1:80',
        error: 'another program already listens on 10.205.0.1:80',
      },
      running: true,
      tasks_alive: true,
      consecutive_sync_failures: 0,
      dropped_log_entries: 0,
    },
  ],
}

describe('DnsPage', () => {
  test('Overview: one health per node, the alert and the verdict', () => {
    const client = createClient()
    seed(client)
    const html = render(client)
    expect(html).toContain('control-plane')
    expect(html).toContain('worker-a')
    expect(html).toContain('The resolver is not running: port 53 is in use')
    expect(html).toContain('Internal DNS not working on 1 node')
    expect(html).toContain('DNS resolver down on worker-a')
    expect(html).toContain('href="/settings/notifications"')
    expect(html).toContain('href="/settings/nodes/7"')
    // Port 80 held by another program is no longer the operator's to free
    // (ADR-098): the agent redirects around it.
    expect(html).not.toContain('Free port 80')
    expect(html).toContain('Main proxy')
    // The cluster-DNS switch is on this tab too.
    expect(html).toContain('Cluster DNS')
    // The other tabs are not rendered until chosen.
    expect(html).not.toContain('staging.shop.temps.local')
    expect(html).not.toContain('production.shop.temps.local')
    client.clear()
  })

  test('master: working lookups never sit as a green badge beside a red error', () => {
    const client = createClient()
    client.setQueryData(dnsKeys.resolvers, masterCase)
    const html = render(client)
    expect(html).toContain('Internal routing down on 1 node')
    expect(html).toContain('Not working')
    expect(html).not.toContain('Healthy')
    expect(html).toContain('Name lookups')
    // The reason is the agent's, shown as reported.
    expect(html).toContain('another program already listens on 10.205.0.1:80')
    // The badge links to master's row in Overview.
    expect(html).toContain('href="/settings/dns?node=4"')
    expect(html).toContain('id="dns-node-4"')
    // The hosted control plane is calm.
    expect(html).toContain('This control plane runs no apps')
    expect(html).not.toContain('Not reporting')
    // Secondary details stay closed until the row is opened.
    expect(html).not.toContain('Listens on')
    client.clear()
  })

  test('?node= opens that row with its details', () => {
    const client = createClient()
    client.setQueryData(dnsKeys.resolvers, masterCase)
    const html = render(client, '/settings/dns?node=4')
    expect(html).toContain('Listens on')
    expect(html).toContain('10.205.0.1:53')
    expect(html).toContain('0.9.1')
    expect(html).toContain(
      'href="/settings/dns?node=4&amp;tab=test&amp;from=4"'
    )
    client.clear()
  })

  test('Queries: logs internal names, withholds forwarded ones, charts traffic', () => {
    const client = createClient()
    seed(client)
    const html = render(client, '/settings/dns?tab=queries')
    expect(html).toContain('staging.shop.temps.local')
    expect(html).toContain('NXDOMAIN')
    expect(html).toContain('SERVFAIL')
    expect(html).toContain('Forwarded name (not logged)')
    expect(html).toContain('shop')
    expect(html).toContain('Internal queries')
    // 2 NXDOMAIN of 20 internal queries.
    expect(html).toContain('10.0%')
    client.clear()
  })

  test('Queries: filters come from the URL', () => {
    const client = createClient()
    seed(client)
    client.setQueryData(
      dnsKeys.queries({
        page: 1,
        page_size: 20,
        outcome: 'nxdomain',
        node_id: 0,
      }),
      { ...queries, items: [queries.items[0]], total: 1 }
    )
    const html = render(
      client,
      '/settings/dns?tab=queries&qresult=nxdomain&qnode=0'
    )
    expect(html).toContain('staging.shop.temps.local')
    expect(html).not.toContain('Forwarded name (not logged)')
    client.clear()
  })

  test('Records: owners, the zone count and a Test link per record', () => {
    const client = createClient()
    seed(client)
    const html = render(client, '/settings/dns?tab=records')
    expect(html).toContain('production.shop.temps.local')
    expect(html).toContain('shop / production')
    expect(html).toMatch(/Records<span[^>]*>12<\/span>/)
    expect(html).toContain(
      'href="/settings/dns?tab=test&amp;name=production.shop.temps.local&amp;type=A&amp;from=0"'
    )
    client.clear()
  })

  test('Test resolution: the fields come prefilled from the URL', () => {
    const client = createClient()
    seed(client)
    const html = render(
      client,
      '/settings/dns?tab=test&name=production.shop.temps.local&type=AAAA&from=7'
    )
    expect(html).toContain('value="production.shop.temps.local"')
    expect(html).toContain('Resolve')
    client.clear()
  })

  test('an unknown tab falls back to Overview', () => {
    const client = createClient()
    seed(client)
    const html = render(client, '/settings/dns?tab=nope')
    expect(html).toContain('Alert: DNS resolver down on worker-a')
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

describe('DnsPage interactions', () => {
  installBrowserWindow()
  afterEach(cleanup)

  test('choosing a tab writes ?tab=, and Overview clears it', async () => {
    const api = mockApi({})
    const client = createClient()
    seed(client)
    const view = renderDom(<Page client={client} />)
    const search = () => view.getByTestId('search').textContent
    expect(search()).toBe('')
    const records = view.getByRole('tab', { name: /Records/ })
    fireEvent.mouseDown(records, { button: 0 })
    fireEvent.click(records)
    await waitFor(() => expect(search()).toBe('?tab=records'))
    await waitFor(() =>
      expect(view.getByText('production.shop.temps.local')).toBeTruthy()
    )
    const overview = view.getByRole('tab', { name: /Overview/ })
    fireEvent.mouseDown(overview, { button: 0 })
    fireEvent.click(overview)
    await waitFor(() => expect(search()).toBe(''))
    api.restore()
    client.clear()
  })

  test("a record's Test link opens the Test tab prefilled", async () => {
    const api = mockApi({})
    const client = createClient()
    seed(client)
    const view = renderDom(
      <Page client={client} url="/settings/dns?tab=records" />
    )
    fireEvent.click(
      view.getByRole('link', { name: 'Test production.shop.temps.local' })
    )
    await waitFor(() =>
      expect(
        view
          .getByRole('tab', { name: /Test resolution/ })
          .getAttribute('aria-selected')
      ).toBe('true')
    )
    const name = view.getByLabelText('Name') as HTMLInputElement
    expect(name.value).toBe('production.shop.temps.local')
    expect(view.getByTestId('search').textContent).toContain('from=0')
    api.restore()
    client.clear()
  })
})
