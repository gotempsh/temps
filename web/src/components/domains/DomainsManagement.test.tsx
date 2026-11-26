// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes, useParams } from 'react-router'
import { toast } from 'sonner'
import { listIngressHostnamesQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  ConnectorView,
  DomainResponse,
  ServiceAccessInfo,
} from '@/api/client/types.gen'
import { PlatformAccessContext } from '@/contexts/PlatformAccessContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { DomainsManagement } from './DomainsManagement'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const domain: DomainResponse = {
  id: 12,
  domain: 'app.example.com',
  status: 'active',
  is_wildcard: false,
  verification_method: 'http-01',
  created_at: new Date(1_767_225_600_000).toISOString(),
  updated_at: new Date(1_767_225_600_000).toISOString(),
  expiration_time: new Date(1_798_761_600_000).toISOString(),
}

function DomainPage() {
  return <p>Opened domain {useParams().id}</p>
}

function renderTable(
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } }),
  routes: Parameters<typeof mockApi>[0] = {},
  accessInfo?: ServiceAccessInfo
) {
  // Tunnels and capabilities answer 404: the table still lists certificates.
  mockApi(routes)
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/domains']}>
        <PlatformAccessContext.Provider
          value={{
            accessInfo,
            isLoading: false,
            error: null,
            refetch: () => {},
            isLocal: false,
            isNat: false,
            isCloudflare: accessInfo?.access_mode === 'cloudflare_tunnel',
            isDirect: accessInfo == null,
          }}
        >
          <Routes>
            <Route
              path="/domains"
              element={
                <DomainsManagement
                  domains={[domain]}
                  sort="domain"
                  direction="asc"
                  onSortChange={() => {}}
                  isError={false}
                  isLoading={false}
                  reloadDomains={() => {}}
                  total={1}
                  page={1}
                  pageSize={20}
                  totalPages={1}
                  onPageChange={() => {}}
                  searchQuery=""
                  onSearchChange={() => {}}
                  isSearching={false}
                />
              }
            />
            <Route path="/domains/:id" element={<DomainPage />} />
          </Routes>
        </PlatformAccessContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the domain name links to the domain, and a click on the row follows it', async () => {
  const view = renderTable()

  const link = view.getByRole('link', { name: 'app.example.com' })
  expect(link.getAttribute('href')).toBe('/domains/12')
  // The name reads as the record's name, not as a link (DESIGN.md).
  expect(link.className.split(' ')).toContain('no-underline')
  expect(link.querySelector('svg')).toBeNull()
  // DESIGN.md: no link overlay across the row; the row follows the link.
  expect(link.className).not.toContain('after:')
  fireEvent.click(link.closest('td') as HTMLElement)
  await waitFor(() => view.getByText('Opened domain 12'))
})

test('a role refused the tunnel reads still lists certificates, with no page failure', async () => {
  const denied = () =>
    jsonResponse({ title: 'Access denied', detail: 'settings:read' }, 403)
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = renderTable(client, {
    'GET /ingress/connectors': denied,
    'GET /ingress/hostnames': denied,
  })

  expect(view.getByRole('link', { name: 'app.example.com' })).toBeTruthy()
  await waitFor(() => {
    const errored = client
      .getQueryCache()
      .findAll()
      .filter((q) => q.state.status === 'error')
    expect(errored.length).toBe(2)
  })
  expect(collectRouteQueryFailures(client)).toEqual([])
})

test('for a role refused the tunnel reads, Refresh succeeds without an error toast', async () => {
  const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
  const denied = () =>
    jsonResponse({ title: 'Access denied', detail: 'settings:read' }, 403)
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = renderTable(client, {
    'GET /ingress/connectors': denied,
    'GET /ingress/hostnames': denied,
  })
  await waitFor(() =>
    expect(
      client
        .getQueryCache()
        .findAll()
        .filter((q) => q.state.status === 'error')
    ).toHaveLength(2)
  )

  fireEvent.click(view.getByRole('button', { name: 'Refresh' }))

  await view.findByText('Updated just now')
  expect(failure).not.toHaveBeenCalled()
})

test('a real failure of the tunnel reads is still reported, not shown as an empty list', async () => {
  const broken = () =>
    jsonResponse({ title: 'Internal Server Error', status: 500 }, 500)
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = renderTable(client, {
    'GET /ingress/connectors': broken,
    'GET /ingress/hostnames': broken,
  })

  expect(view.getByRole('link', { name: 'app.example.com' })).toBeTruthy()
  await waitFor(() => {
    const errored = client
      .getQueryCache()
      .findAll()
      .filter((q) => q.state.status === 'error')
    expect(errored.length).toBe(2)
  })
  expect(collectRouteQueryFailures(client)).toHaveLength(2)
})

test('retry targets one failed hostname and refreshes its row after a conflict or success', async () => {
  const conflict = spyOn(toast, 'error').mockImplementation(() => 'id')
  const success = spyOn(toast, 'success').mockImplementation(() => 'id')
  const hostname = 'shop.example.com'
  const failed = {
    hostname,
    connector_id: 9,
    retry_dns_connector_id: 9,
    state: 'error',
    detail: 'DNS record is managed elsewhere',
  }
  const published = {
    ...failed,
    retry_dns_connector_id: null,
    state: 'published',
  }
  let current: typeof failed | typeof published = failed
  let finish = (_response: Response) => {}
  let reads = 0
  let writes = 0
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = renderTable(client, {
    'GET /ingress/connectors': () => jsonResponse({ connectors: [], total: 0 }),
    'GET /ingress/hostnames': () => {
      reads++
      return jsonResponse([current])
    },
    'POST /ingress/connectors/9/hostnames/shop.example.com/retry-dns': () => {
      writes++
      return new Promise((resolve) => {
        finish = resolve
      })
    },
  })
  try {
    fireEvent.click(
      await view.findByRole('button', { name: 'Retry DNS setup' })
    )
    expect(
      (
        (await view.findByRole('button', {
          name: 'Retrying DNS setup…',
        })) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    await waitFor(() => expect(writes).toBe(1))
    finish(jsonResponse(failed))
    await waitFor(() =>
      expect(conflict).toHaveBeenCalledWith(
        `DNS setup failed for ${hostname}`,
        { description: failed.detail }
      )
    )
    await waitFor(() => expect(reads).toBeGreaterThan(1))
    fireEvent.click(
      await view.findByRole('button', { name: 'Retry DNS setup' })
    )
    await waitFor(() => expect(writes).toBe(2))
    current = published
    await client.invalidateQueries({ queryKey: listIngressHostnamesQueryKey() })
    await view.findByText('Live')
    expect(
      (
        view.getByRole('button', {
          name: 'Retrying DNS setup…',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    finish(jsonResponse(current))
    await waitFor(() =>
      expect(
        view.queryByRole('button', { name: 'Retrying DNS setup…' })
      ).toBeNull()
    )
    expect(success).toHaveBeenCalledWith(`DNS setup completed for ${hostname}`)
    expect(view.queryByRole('button', { name: 'Retry DNS setup' })).toBeNull()
  } finally {
    success.mockRestore()
  }
})

/** A tunnel on one domain; the operator's other domains are served directly. */
const tunnel: ConnectorView = {
  id: 9,
  name: 'edge',
  provider: 'cloudflare_tunnel',
  domain: 'apps.example.org',
  status: 'ready',
  auto_publish: false,
  environment_hosts: false,
  edge_port: 443,
  created_at: '2026-01-01T00:00:00Z',
  nodes: [{ node_id: 3, node_name: 'worker-1', state: 'ready' }],
  publications: [],
}

/** The console opened through the tunnel: `cf-ray` on the request. */
const openedThroughCloudflare: ServiceAccessInfo = {
  access_mode: 'cloudflare_tunnel',
  public_ip: '203.0.113.10',
  can_create_domains: true,
}

test('opened through a Cloudflare Tunnel, Add domain is still offered', async () => {
  const view = renderTable(
    undefined,
    {
      'GET /ingress/connectors': () =>
        jsonResponse({ connectors: [tunnel], total: 1 }),
      'GET /ingress/hostnames': () =>
        jsonResponse([
          {
            hostname: 'my-app.apps.example.org',
            connector_id: 9,
            connector_name: 'edge',
            state: 'published',
            mode: 'tunnel',
            detail: '',
            candidates: [],
            served_by_workers: true,
          },
        ]),
    },
    openedThroughCloudflare
  )

  const add = view.getByRole('link', { name: /^Add domain/ })
  expect(add.getAttribute('href')).toBe('/domains/add')
  expect(add.getAttribute('aria-disabled')).toBeNull()
  expect(view.queryByText('Managed by Cloudflare')).toBeNull()
  expect(view.queryByText(/through your Cloudflare dashboard/)).toBeNull()

  // The status line covers only what the tunnel serves.
  const tunnels = await view.findByRole('list', { name: 'Tunnels' })
  await waitFor(() =>
    expect(tunnels.textContent).toContain(
      'Tunnel edge serves names under apps.example.org · 1 published'
    )
  )
  expect(tunnels.textContent).toContain(
    'Other domains get HTTPS from a Temps certificate.'
  )
  // A certificate domain beside it keeps its certificate actions.
  fireEvent.pointerDown(
    view.getByRole('button', { name: 'Actions for app.example.com' }),
    { button: 0, ctrlKey: false }
  )
  await view.findByText('Renew certificate')
})

test('without a tunnel there is no tunnel status line', async () => {
  const view = renderTable(undefined, {
    'GET /ingress/connectors': () => jsonResponse({ connectors: [], total: 0 }),
    'GET /ingress/hostnames': () => jsonResponse([]),
  })
  expect(view.getByRole('link', { name: /^Add domain/ })).toBeTruthy()
  await waitFor(() =>
    expect(view.queryByRole('list', { name: 'Tunnels' })).toBeNull()
  )
})
