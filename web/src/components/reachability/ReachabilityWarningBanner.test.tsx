// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import type { ReachabilityWarning } from '@/api/client'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import {
  ReachabilityWarningBanner,
  tunnelSuggestions,
} from './ReachabilityWarningBanner'

installBrowserWindow()
afterEach(cleanup)
beforeEach(() => localStorage.clear())
const warning: ReachabilityWarning = {
  environment_id: 4,
  environment: 'app-production',
  headline: 'Old warning',
  reason: 'Only resolves locally.',
  fixes: [
    {
      kind: 'tunnel_environment_hosts',
      label: 'Old tunnel offer',
      hostname: 'app-production.example.com',
      connector_id: 2,
    },
    {
      kind: 'preview_domain',
      label: 'Set a wildcard preview domain',
      href: '/settings#preview-domain',
    },
    {
      kind: 'worker_ingress',
      label: 'Let a worker take public traffic',
      command: 'temps agent ingress delegated',
    },
  ],
}
function renderBanner(
  value: ReachabilityWarning | null = warning,
  projectId?: number
) {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ReachabilityWarningBanner
          warning={value}
          projectSlug="shop"
          projectId={projectId}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}
test('one solution-first action with editable label and quiet explanation', () => {
  const view = renderBanner()
  expect(view.container.textContent).toContain('Make app-production reachable:')
  expect(view.container.textContent).not.toContain('Old warning')
  expect(view.container.textContent).not.toContain('Old tunnel offer')
  expect(view.container.querySelector('details')?.open).toBe(false)
  expect(view.container.querySelector('code')).toBeNull()
  fireEvent.change(view.getByLabelText('Subdomain'), {
    target: { value: 'store' },
  })
  expect(view.getByText('Add domain').getAttribute('href')).toContain(
    'address=store.example.com'
  )
  expect(view.getByText('Add domain').getAttribute('href')).toContain(
    'way=existing_tunnel'
  )
})
test('no tunnel has one setup link; no warning renders nothing', () => {
  const view = renderBanner({ ...warning, fixes: [] })
  expect(view.container.querySelectorAll('button').length).toBe(1)
  expect(view.container.querySelector('a')?.textContent).toBe(
    'Make it reachable'
  )
  view.unmount()
  expect(renderBanner(null).container.textContent).toBe('')
})
test('Not now persists per environment without hiding another environment', () => {
  const view = renderBanner()
  fireEvent.click(view.getByText('Not now'))
  expect(view.container.textContent).toBe('')
  view.unmount()
  const again = renderBanner()
  expect(again.container.textContent).toBe('')
  again.unmount()
  expect(
    renderBanner({ ...warning, environment_id: 5 }).container.textContent
  ).toContain('Add domain')
})
test('several zones preserve the server preference order', () => {
  const second = {
    ...warning.fixes[0],
    hostname: 'app-production.example.org',
    connector_id: 3,
  }
  const value = { ...warning, fixes: [...warning.fixes, second] }
  expect(tunnelSuggestions(value).map((f) => f.connector_id)).toEqual([2, 3])
  const view = renderBanner(value)
  expect(view.getByLabelText('Tunnel zone').textContent).toContain(
    '.example.com'
  )
})
type Call = { method: string; url: string; body: any }
/** Answers the SDK's requests from `answer`, recording each one. */
function mockServer(
  answer: (call: Call) => { status?: number; body: unknown }
): { calls: Call[]; restore: () => void } {
  const previous = client.getConfig()
  const calls: Call[] = []
  client.setConfig({
    baseUrl: 'http://localhost',
    fetch: (async (request: RequestInfo | URL) => {
      const req = request as Request
      const body = req.method === 'POST' ? await req.json() : null
      const call = { method: req.method, url: req.url, body }
      calls.push(call)
      const { status = 200, body: result } = answer(call)
      return new Response(JSON.stringify(result), {
        status,
        headers: { 'Content-Type': 'application/json' },
      })
    }) as typeof fetch,
  })
  // setConfig merges, so restoring `previous` alone keeps this fetch when the
  // original config had none, and later test files that mock globalThis.fetch
  // would reach this fake server. Name both keys so they are reset too.
  return {
    calls,
    restore: () =>
      client.setConfig({
        ...previous,
        baseUrl: previous.baseUrl,
        fetch: previous.fetch,
      }),
  }
}
const tunnelOption = (extra: Record<string, unknown> = {}) => ({
  kind: 'existing_tunnel',
  recommended: true,
  way_in: 'tunnel:homelab',
  fields: [],
  steps: [],
  warnings: [],
  confirmation: null,
  ...extra,
})
const run = (state: string, steps: unknown[] = []) => ({
  id: 8,
  state,
  live_url: state === 'succeeded' ? 'https://store.example.com' : null,
  steps,
  journal: [],
  created: [],
  undo: [],
})
/** Where the router is, so a test can prove nothing navigated. */
function Location() {
  const location = useLocation()
  return <output data-testid="location">{location.pathname}</output>
}
function renderWithLocation() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/projects/shop']}>
        <ReachabilityWarningBanner
          warning={warning}
          projectSlug="shop"
          projectId={3}
        />
        <Location />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('one click adds the domain and shows its live URL in place', async () => {
  const server = mockServer(({ url }) => ({
    body: url.endsWith('/plan')
      ? { plan_id: 'p1', options: [tunnelOption()] }
      : run('succeeded', [
          { kind: 'attach_domain', label: 'Attach', status: 'done' },
          { kind: 'wait_for_publication', label: 'Publish', status: 'done' },
        ]),
  }))
  try {
    const view = renderWithLocation()
    fireEvent.change(view.getByLabelText('Subdomain'), {
      target: { value: 'store' },
    })
    fireEvent.click(view.getByText('Add domain'))
    await waitFor(() => expect(view.container.textContent).toContain('Live at'))
    const live = view.getByText('https://store.example.com')
    expect(live.getAttribute('href')).toBe('https://store.example.com')
    expect(view.container.textContent).toContain(
      'HTTPS by Cloudflare · DNS published'
    )
    // The settings page is a secondary link, never a step.
    expect(view.getAllByText('Domain settings')[0].getAttribute('href')).toBe(
      '/projects/shop/settings/domains'
    )
    expect(view.container.textContent).not.toContain('Review domain setup')
    expect(view.getByTestId('location').textContent).toBe('/projects/shop')
    const posts = server.calls.filter((c) => c.method === 'POST')
    expect(posts[0].body.address).toBe('store.example.com')
    expect(posts[1].body).toEqual({
      plan_id: 'p1',
      option: 'existing_tunnel',
      answers: { address: 'store.example.com' },
      confirmed: false,
    })
    expect(posts.length).toBe(2)
  } finally {
    server.restore()
  }
})

test('a tunnel that publishes only chosen hostnames still takes one click', async () => {
  // The plan routes the hostname through the tunnel (two actions) and,
  // with nothing chosen for it yet, asks no confirmation.
  const server = mockServer(({ method, url }) => ({
    body: url.endsWith('/plan')
      ? {
          plan_id: 'p3',
          options: [
            {
              ...tunnelOption(),
              kind: 'tunnel_environment_hosts',
              confirmation: 'Every environment',
            },
            tunnelOption({
              recommended: false,
              actions: 2,
              steps: [{ kind: 'route_hostname', label: 'Route' }],
            }),
          ],
        }
      : method === 'POST'
        ? run('running', [
            {
              kind: 'route_hostname',
              label: 'Route through homelab',
              status: 'running',
            },
          ])
        : run('succeeded'),
  }))
  try {
    const view = renderWithLocation()
    fireEvent.click(view.getByText('Add domain'))
    await waitFor(() => expect(view.container.textContent).toContain('Live at'))
    const apply = server.calls.find((c) => c.url.endsWith('/apply'))
    expect(apply?.body.option).toBe('existing_tunnel')
    expect(apply?.body.confirmed).toBe(false)
    expect(view.getByTestId('location').textContent).toBe('/projects/shop')
  } finally {
    server.restore()
  }
})

test('a plan that changes someone else’s choice asks inline, then applies', async () => {
  const server = mockServer(({ url }) => ({
    body: url.endsWith('/plan')
      ? {
          plan_id: 'p2',
          options: [
            tunnelOption({
              confirmation: 'Route it: it is set to no tunnel now.',
            }),
          ],
        }
      : run('succeeded'),
  }))
  try {
    const view = renderWithLocation()
    fireEvent.click(view.getByText('Add domain'))
    await waitFor(() =>
      expect(view.container.textContent).toContain('it is set to no tunnel now')
    )
    expect(server.calls.length).toBe(1)
    expect(view.container.textContent).not.toContain('Review domain setup')
    fireEvent.click(view.getByText('Add anyway'))
    await waitFor(() => expect(view.container.textContent).toContain('Live at'))
    const apply = server.calls.find((c) => c.url.endsWith('/apply'))
    expect(apply?.body).toMatchObject({ plan_id: 'p2', confirmed: true })
    // Confirming reuses the plan instead of planning again.
    expect(server.calls.filter((c) => c.url.endsWith('/plan')).length).toBe(1)
    expect(view.getByTestId('location').textContent).toBe('/projects/shop')
  } finally {
    server.restore()
  }
})

test('a refused apply shows the server’s reason in place and can retry', async () => {
  const server = mockServer(({ url }) =>
    url.endsWith('/plan')
      ? { body: { plan_id: 'p4', options: [tunnelOption()] } }
      : {
          status: 400,
          body: {
            title: 'Bad request',
            detail: 'Tunnel homelab is not ready.',
          },
        }
  )
  try {
    const view = renderWithLocation()
    fireEvent.click(view.getByText('Add domain'))
    await waitFor(() =>
      expect(view.getByRole('alert').textContent).toContain(
        'Could not add app-production.example.com: Tunnel homelab is not ready.'
      )
    )
    fireEvent.click(view.getByText('Try again'))
    expect(view.getByText('Add domain')).toBeTruthy()
    expect(
      (view.getByLabelText('Subdomain') as HTMLInputElement).disabled
    ).toBe(false)
  } finally {
    server.restore()
  }
})

test('a plan with no per-domain way in says why without navigating', async () => {
  const server = mockServer(() => ({
    body: {
      plan_id: null,
      options: [],
      blocked: { reason: 'Tunnel homelab covers it but runs on no worker.' },
    },
  }))
  try {
    const view = renderWithLocation()
    fireEvent.click(view.getByText('Add domain'))
    await waitFor(() =>
      expect(view.getByRole('alert').textContent).toContain('runs on no worker')
    )
    expect(view.getByTestId('location').textContent).toBe('/projects/shop')
  } finally {
    server.restore()
  }
})

test('invalid labels cannot publish and public ingress offers Add domain', () => {
  const view = renderBanner(warning, 3)
  fireEvent.change(view.getByLabelText('Subdomain'), {
    target: { value: 'two.labels' },
  })
  expect(view.getByText('Add domain').getAttribute('aria-disabled')).toBe(
    'true'
  )
  view.unmount()
  const direct = renderBanner({
    ...warning,
    fixes: [
      {
        kind: 'custom_domain',
        label: 'Add domain',
        href: '/projects/shop/settings/domains',
      },
    ],
  })
  expect(direct.getByText('Add domain').getAttribute('href')).toBe(
    '/projects/shop/settings/domains'
  )
})
