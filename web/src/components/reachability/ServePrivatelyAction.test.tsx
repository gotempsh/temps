// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { PrivateNetworkOption, ReachAction } from '@/api/client'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import { ServePrivatelyAction } from './ServePrivatelyAction'

installBrowserWindow()
afterEach(cleanup)

const action: ReachAction = {
  kind: 'serve_privately',
  label: 'Serve private addresses from this machine',
  command:
    "bunx @temps-sdk/cli ingress private --listen <this machine's private address, or tailscale>",
}

const tailscale: PrivateNetworkOption = {
  interface: 'tailscale0',
  address: '100.101.102.103',
  tailscale: true,
  admit: ['100.64.0.0/10', 'fd7a:115c:a1e0::/48'],
  problem: null,
}
const wireguard: PrivateNetworkOption = {
  interface: 'wg0',
  address: '10.8.0.1',
  tailscale: false,
  admit: ['10.8.0.0/24'],
  problem: null,
}
const bridged: PrivateNetworkOption = {
  interface: 'eth1',
  address: '172.18.0.5',
  tailscale: false,
  admit: [],
  problem:
    '172.18.0.0/16 includes the container network app-net (172.18.0.0/16): any app’s container could reach private addresses.',
}

type Call = { method: string; url: string; body: any }

function mockServer(
  networks: PrivateNetworkOption[],
  put: { status: number; body: unknown } = {
    status: 200,
    body: {
      listen: 'tailscale',
      tailscale_serve: false,
      updated_at: '2026-11-24T10:00:00Z',
    },
  }
) {
  const previous = client.getConfig()
  const calls: Call[] = []
  client.setConfig({
    baseUrl: 'http://localhost',
    fetch: (async (request: RequestInfo | URL) => {
      const req = request as Request
      const body = req.method === 'PUT' ? await req.json() : null
      calls.push({ method: req.method, url: req.url, body })
      const answer =
        req.method === 'PUT'
          ? put
          : {
              status: 200,
              body: {
                consent: null,
                serving: null,
                admit: [],
                error: null,
                reported_at: null,
                networks,
              },
            }
      return new Response(JSON.stringify(answer.body), {
        status: answer.status,
        headers: { 'Content-Type': 'application/json' },
      })
    }) as typeof fetch,
  })
  return {
    calls,
    puts: () => calls.filter((c) => c.method === 'PUT'),
    restore: () =>
      client.setConfig({
        ...previous,
        baseUrl: previous.baseUrl,
        fetch: previous.fetch,
      }),
  }
}

function renderAction(queryClient = new QueryClient()) {
  return render(
    <QueryClientProvider client={queryClient}>
      <ServePrivatelyAction
        action={action}
        replanKey={['reachabilityPlan']}
        applyDelayMs={0}
      />
    </QueryClientProvider>
  )
}

test('with one private network, one click serves on it and plans again', async () => {
  const server = mockServer([tailscale, bridged])
  const queryClient = new QueryClient()
  queryClient.setQueryData(['reachabilityPlan', 1, 2], { stale: false })
  try {
    const view = renderAction(queryClient)
    const button = await view.findByText(
      'Serve private addresses from this machine on Tailscale (100.101.102.103)'
    )
    expect(view.container.textContent).toContain(action.command!)
    fireEvent.click(button)
    await waitFor(() =>
      expect(
        queryClient.getQueryState(['reachabilityPlan', 1, 2])?.isInvalidated
      ).toBe(true)
    )
    expect(server.puts()).toHaveLength(1)
    expect(server.puts()[0].url).toEndWith('/settings/private-ingress')
    expect(server.puts()[0].body).toEqual({
      listen: 'tailscale',
      tailscale_serve: false,
    })
  } finally {
    server.restore()
  }
})

test('with several networks it asks which, and says why one cannot be used', async () => {
  const server = mockServer([tailscale, wireguard, bridged])
  try {
    const view = renderAction()
    await view.findByText('Which private network should this machine serve on?')
    expect(view.container.textContent).toContain('Only 10.8.0.0/24 can connect')
    expect(view.container.textContent).toContain(
      'includes the container network app-net'
    )
    const buttons = view.getAllByText('Serve on this network')
    expect(buttons).toHaveLength(2)
    fireEvent.click(buttons[1])
    await waitFor(() => expect(server.puts()).toHaveLength(1))
    expect(server.puts()[0].body).toEqual({
      listen: '10.8.0.1',
      tailscale_serve: false,
    })
  } finally {
    server.restore()
  }
})

test('a machine on no private network is told to join one', async () => {
  const server = mockServer([])
  try {
    const view = renderAction()
    await view.findByText(/This machine is on no private network/)
  } finally {
    server.restore()
  }
})

test('a refusal is shown in place', async () => {
  const server = mockServer([wireguard], {
    status: 400,
    body: {
      title: 'Invalid Private Ingress Address',
      detail: 'listen: 10.8.0.1 is not an address of this machine',
      status: 400,
    },
  })
  try {
    const view = renderAction()
    fireEvent.click(
      await view.findByText(
        'Serve private addresses from this machine on wg0 (10.8.0.1)'
      )
    )
    await waitFor(() =>
      expect(view.container.textContent).toContain(
        'is not an address of this machine'
      )
    )
  } finally {
    server.restore()
  }
})
