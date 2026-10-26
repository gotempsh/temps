// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { IngressSetupPage } from './IngressSetupPage'

afterEach(cleanup)

const worker = (id: number, name: string, status = 'active') => ({
  id,
  name,
  status,
  role: 'worker',
  address: '10.0.0.' + id,
  private_address: '10.0.0.' + id,
  created_at: '2026-01-01T00:00:00Z',
  last_heartbeat: '2026-01-01T00:00:00Z',
  labels: {},
  capacity: {},
  build_limits: {
    concurrency: 1,
    queue_wait_seconds: 300,
    build_timeout_seconds: 1800,
  },
  public_ingress_enabled: false,
  public_ingress_unsupported_reasons: [],
})

const settings = (nodeId: number, mode: string) => ({
  node_id: nodeId,
  mode,
  agent_reports: true,
  behind_nat: false,
  enabled: false,
  http_port: 80,
  https_port: 443,
  interfaces: [],
})

const tunnel = (state: string) => ({
  id: 9,
  name: 'example-com',
  provider: 'cloudflare_tunnel',
  domain: 'example.com',
  status: 'ready',
  auto_publish: true,
  edge_port: 8080,
  created_at: '2026-01-01T00:00:00Z',
  publications: [],
  nodes: [{ node_id: 1, node_name: 'worker-a', state, ready_connections: 4 }],
})

const cloudflareAccount = (id: number, name: string) => ({
  id,
  name,
  provider_type: 'cloudflare',
  is_active: true,
  credentials: {},
  flat_hostnames_supported: true,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
})

function setup() {
  const posted: { path: string; body: unknown }[] = []
  // A pasted token adds account 2 next to the existing account 1.
  const accounts = [cloudflareAccount(1, 'Cloudflare')]
  const perAccount = (id: number) => ({
    [`GET /dns-providers/${id}/zones`]: () =>
      jsonResponse({
        zones: [
          { id: 'z1', name: 'example.com', status: 'active', nameservers: [] },
        ],
      }),
    [`GET /dns-providers/${id}/domains`]: () => jsonResponse([]),
    [`POST /dns-providers/${id}/domains`]: async (request: Request) => {
      posted.push({ path: 'domains', body: await request.json() })
      return jsonResponse(managedDomain(false), 201)
    },
    [`POST /dns-providers/${id}/domains/example.com/verify`]: () =>
      jsonResponse(managedDomain(true)),
  })
  const api = mockApi({
    ...perAccount(1),
    ...perAccount(2),
    'GET /internal/nodes': () =>
      jsonResponse({
        nodes: [
          worker(1, 'worker-a'),
          worker(2, 'worker-b'),
          worker(3, 'worker-c', 'offline'),
        ],
      }),
    'GET /internal/nodes/1/public-ingress/settings': () =>
      jsonResponse(settings(1, 'delegated')),
    'GET /internal/nodes/2/public-ingress/settings': () =>
      jsonResponse(settings(2, 'off')),
    'GET /ingress/providers': () =>
      jsonResponse([
        {
          kind: 'cloudflare_tunnel',
          name: 'Cloudflare Tunnel',
          summary: '',
          requirements: [],
          fields: [],
          needs_public_ip: false,
          works_behind_cgnat: true,
          dns: 'automatic',
        },
      ]),
    'GET /ingress/connectors': () =>
      jsonResponse({ connectors: [], total: 0, page: 1, page_size: 100 }),
    'GET /dns-providers': () => jsonResponse(accounts),
    'POST /dns-providers': () => {
      const added = cloudflareAccount(2, 'Cloudflare 2')
      accounts.push(added)
      return jsonResponse(added, 201)
    },
    'DELETE /dns-providers/2': () => {
      accounts.splice(1)
      return new Response(null, { status: 204 })
    },
    'POST /ingress/connectors': async (request) => {
      posted.push({ path: 'connectors', body: await request.json() })
      return jsonResponse(tunnel('pending'), 201)
    },
    'GET /ingress/connectors/9': () => jsonResponse(tunnel('ready')),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/settings/nodes/ingress']}>
        <BreadcrumbProvider>
          <IngressSetupPage />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, posted }
}

function managedDomain(verified: boolean) {
  return {
    id: 3,
    provider_id: 1,
    domain: 'example.com',
    auto_manage: true,
    verified,
    proxied_by_default: false,
    sync_generated_records: false,
    generated_hostname_mode: 'standard',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  }
}

test('defaults to a tunnel on the workers with no way in, and sets it up step by step', async () => {
  const { posted } = setup()
  await screen.findByText('example.com')
  // Recommended: no worker has a public address.
  expect(screen.getByText('Recommended')).toBeTruthy()

  const a = screen.getByRole('checkbox', { name: 'worker-a' })
  const b = screen.getByRole('checkbox', { name: 'worker-b' })
  const c = screen.getByRole('checkbox', { name: 'worker-c' })
  await waitFor(() => expect(a.getAttribute('data-state')).toBe('checked'))
  // B refuses public traffic, C is offline: neither can be picked.
  await waitFor(() => expect(b.hasAttribute('disabled')).toBe(true))
  expect(c.hasAttribute('disabled')).toBe(true)
  expect(screen.getByText('temps agent ingress delegated')).toBeTruthy()

  fireEvent.click(screen.getByRole('radio', { name: /example\.com/ }))
  // The subdomain scheme warns about Universal SSL before it can be used.
  fireEvent.click(screen.getByRole('radio', { name: /Under a subdomain/ }))
  expect(
    screen.getByText('HTTPS fails without Advanced Certificate Manager')
  ).toBeTruthy()
  fireEvent.click(screen.getByRole('radio', { name: /app\.example\.com/ }))

  const create = screen.getByRole('button', { name: 'Create tunnel' })
  await waitFor(() => expect(create.hasAttribute('disabled')).toBe(false))
  fireEvent.click(create)

  await screen.findByText('Visitors can reach apps under example.com')
  expect(screen.getByText('Connected on worker-a')).toBeTruthy()
  expect(screen.getByText('4 connections to Cloudflare')).toBeTruthy()
  expect(posted).toEqual([
    { path: 'domains', body: { domain: 'example.com', auto_manage: true } },
    {
      path: 'connectors',
      body: {
        name: 'example-com',
        provider: 'cloudflare_tunnel',
        values: { dns_provider_id: 1, domain: 'example.com' },
        node_ids: [1],
      },
    },
  ])
})

test('open ports hands over to each worker’s own page', async () => {
  setup()
  fireEvent.click(
    await screen.findByRole('radio', { name: /Open ports on a public IP/ })
  )
  const link = await screen.findByRole('link', {
    name: 'Set up open ports on worker-a',
  })
  expect(link.getAttribute('href')).toBe('/settings/nodes/1#public-ingress')
})

async function connectToken() {
  fireEvent.click(
    await screen.findByRole('radio', { name: /Paste an API token/ })
  )
  fireEvent.change(await screen.findByLabelText('API token'), {
    target: { value: 'cf-token' },
  })
  fireEvent.click(
    screen.getByRole('button', { name: 'Check token and continue' })
  )
  return screen.findByRole('button', { name: 'Use a different token' })
}

test('replacing a pasted token asks before removing the account it added', async () => {
  const { api } = setup()
  fireEvent.click(await connectToken())

  const dialog = await screen.findByRole('alertdialog')
  expect(dialog.textContent).toContain(
    'Remove Cloudflare account Cloudflare 2?'
  )
  fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
  await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
  expect(api.called('DELETE', '/dns-providers/2')).toBe(false)

  fireEvent.click(screen.getByRole('button', { name: 'Use a different token' }))
  fireEvent.click(await screen.findByRole('button', { name: 'Remove' }))
  await screen.findByRole('button', { name: 'Check token and continue' })
  expect(api.called('DELETE', '/dns-providers/2')).toBe(true)
})

test('after a run, the account its tunnel uses is no longer offered for removal', async () => {
  const { api } = setup()
  await connectToken()
  fireEvent.click(await screen.findByRole('radio', { name: /example\.com/ }))
  const create = screen.getByRole('button', { name: 'Create tunnel' })
  await waitFor(() => expect(create.hasAttribute('disabled')).toBe(false))
  fireEvent.click(create)
  await screen.findByText('Visitors can reach apps under example.com')

  fireEvent.click(screen.getByRole('button', { name: 'Set up another' }))
  // The account is now an existing one, picked, with nothing to delete.
  await waitFor(() =>
    expect(
      screen
        .getByRole('radio', { name: 'Cloudflare 2' })
        .getAttribute('data-state')
    ).toBe('checked')
  )
  expect(
    screen.queryByRole('button', { name: 'Use a different token' })
  ).toBeNull()
  expect(api.called('DELETE', '/dns-providers/2')).toBe(false)
})
