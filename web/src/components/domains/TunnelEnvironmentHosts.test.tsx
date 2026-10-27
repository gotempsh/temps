// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { getConnectorEnvironmentHostsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  ConnectorView,
  EnvironmentHostsResponse,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import {
  TunnelEnvironmentHostsView,
  WithdrawnEnvironmentHosts,
} from './TunnelEnvironmentHosts'

installBrowserWindow()
afterEach(cleanup)

const off: EnvironmentHostsResponse = {
  connector_id: 2,
  connector_name: 'edge',
  domain: 'apps.example.com',
  enabled: false,
  hosts: [
    {
      environment_id: 4,
      project_slug: 'shop',
      environment_name: 'production',
      hostname: 'shop-production.apps.example.com',
      state: 'planned',
      last_error: null,
      note: null,
    },
    {
      environment_id: 5,
      project_slug: 'shop',
      environment_name: 'feature-login',
      hostname: 'shop-feature-login.apps.example.com',
      state: 'planned',
      last_error: null,
      note: null,
    },
  ],
  refused: [
    {
      environment_id: 6,
      project_slug: 'admin',
      environment_name: 'production',
      reason: 'It is private or for signed-in people only.',
    },
  ],
  enabled_elsewhere: null,
  preview_domain_usable: false,
  warning: 'Branch names become public.',
  confirm_token: '3f2a9c1e0b7d4a65',
}

test('turning it on lists every host and the branch warning before anything changes', () => {
  const calls: boolean[] = []
  render(
    <TunnelEnvironmentHostsView
      data={off}
      pending={false}
      onConfirm={(enabled) => calls.push(enabled)}
    />
  )
  // Nothing is listed or sent before the user asks.
  expect(screen.queryByText('shop-production.apps.example.com')).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: 'Publish environments' }))
  expect(calls).toEqual([])
  expect(screen.getByText('Branch names become public.')).toBeTruthy()
  expect(screen.getByText('shop-feature-login.apps.example.com')).toBeTruthy()
  expect(screen.getByText(/admin \/ production: It is private/)).toBeTruthy()
  fireEvent.click(screen.getByRole('button', { name: 'Publish them' }))
  expect(calls).toEqual([true])
})

test('turning it off asks before withdrawing the hosts', () => {
  const calls: boolean[] = []
  const on: EnvironmentHostsResponse = {
    ...off,
    enabled: true,
    hosts: off.hosts.map((h) => ({ ...h, state: 'published' })),
  }
  render(
    <TunnelEnvironmentHostsView
      data={on}
      pending={false}
      onConfirm={(enabled) => calls.push(enabled)}
    />
  )
  expect(screen.getAllByText('Live')).toHaveLength(2)
  fireEvent.click(
    screen.getByRole('button', { name: 'Stop publishing environments' })
  )
  expect(calls).toEqual([])
  fireEvent.click(screen.getByRole('button', { name: 'Withdraw them' }))
  expect(calls).toEqual([false])
})

test('another tunnel already publishing environments blocks this one and says which', () => {
  render(
    <TunnelEnvironmentHostsView
      data={{ ...off, enabled_elsewhere: 'homelab' }}
      pending={false}
      onConfirm={() => {}}
    />
  )
  const button = screen.getByRole('button', {
    name: 'Publish environments',
  }) as HTMLButtonElement
  expect(button.disabled).toBe(true)
  expect(screen.getByText(/Tunnel homelab already does this/)).toBeTruthy()
})

function connector(environment_hosts: boolean): ConnectorView {
  return {
    id: 2,
    name: 'edge',
    provider: 'cloudflare_tunnel',
    domain: 'apps.example.com',
    status: 'ready',
    edge_port: 8080,
    auto_publish: false,
    environment_hosts,
    nodes: [],
    publications: [],
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  } as ConnectorView
}

function renderWithdrawn(value: ConnectorView) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  queryClient.setQueryData(
    getConnectorEnvironmentHostsQueryKey({ path: { id: 2 } }),
    { ...off, enabled: true }
  )
  return render(
    <QueryClientProvider client={queryClient}>
      <WithdrawnEnvironmentHosts connector={value} />
    </QueryClientProvider>
  )
}

test('deleting a tunnel names the environment addresses it withdraws', () => {
  const { container } = renderWithdrawn(connector(true))
  expect(container.textContent).toContain('stop working with it')
  expect(container.textContent).toContain('shop-production.apps.example.com')
})

test('a tunnel without environment hosts adds nothing to its delete dialog', () => {
  const { container } = renderWithdrawn(connector(false))
  expect(container.textContent).toBe('')
})
