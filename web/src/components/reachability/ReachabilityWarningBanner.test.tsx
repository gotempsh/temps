// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
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
test('one click plans the edited host and applies only the per-domain option', async () => {
  const previous = client.getConfig()
  const calls: { url: string; body: any }[] = []
  client.setConfig({
    baseUrl: 'http://localhost',
    fetch: (async (request: RequestInfo | URL) => {
      const req = request as Request
      const body = req.method === 'POST' ? await req.json() : null
      calls.push({ url: req.url, body })
      const result = req.url.endsWith('/plan')
        ? {
            plan_id: 'p1',
            options: [
              {
                kind: 'existing_tunnel',
                recommended: true,
                fields: [],
                confirmation: null,
              },
            ],
          }
        : { id: 8, state: 'succeeded', live_url: 'https://store.example.com' }
      return new Response(JSON.stringify(result), {
        headers: { 'Content-Type': 'application/json' },
      })
    }) as typeof fetch,
  })
  try {
    const view = renderBanner(warning, 3)
    fireEvent.change(view.getByLabelText('Subdomain'), {
      target: { value: 'store' },
    })
    fireEvent.click(view.getByText('Add domain'))
    await waitFor(() => expect(view.container.textContent).toContain('Live at'))
    expect(calls[0].body.address).toBe('store.example.com')
    expect(calls[1].body).toEqual({
      plan_id: 'p1',
      option: 'existing_tunnel',
      answers: { address: 'store.example.com' },
      confirmed: false,
    })
  } finally {
    client.setConfig(previous)
  }
})

test('confirmation-required plans go to review without applying', async () => {
  const previous = client.getConfig()
  let requests = 0
  client.setConfig({
    baseUrl: 'http://localhost',
    fetch: (async (_request: RequestInfo | URL) => {
      requests++
      return new Response(
        JSON.stringify({
          plan_id: 'p2',
          options: [
            {
              kind: 'existing_tunnel',
              fields: [],
              confirmation: 'Replace another route',
            },
          ],
        }),
        { headers: { 'Content-Type': 'application/json' } }
      )
    }) as typeof fetch,
  })
  try {
    const view = renderBanner(warning, 3)
    fireEvent.click(view.getByText('Add domain'))
    await waitFor(() =>
      expect(view.container.textContent).toContain('Review domain setup')
    )
    expect(requests).toBe(1)
  } finally {
    client.setConfig(previous)
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
