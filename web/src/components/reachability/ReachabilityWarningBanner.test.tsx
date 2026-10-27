// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ReachabilityWarning } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { ReachabilityWarningBanner } from './ReachabilityWarningBanner'

installBrowserWindow()
afterEach(cleanup)

const warning: ReachabilityWarning = {
  environment_id: 4,
  environment: 'production',
  headline: 'Deployed, but nobody can reach production yet',
  reason: 'app-production.localho.st only resolves on this machine.',
  fixes: [
    {
      kind: 'tunnel_environment_hosts',
      label: "Publish it through tunnel 'edge' as app-production.example.com",
      href: '/settings/nodes/ingress',
      command: null,
      connector_id: 2,
      hostname: 'app-production.example.com',
    },
    {
      kind: 'preview_domain',
      label: 'Set a wildcard preview domain in Settings → Platform',
      href: '/settings#preview-domain',
      command: null,
      connector_id: null,
      hostname: null,
    },
    {
      kind: 'worker_ingress',
      label: 'Let a worker take public traffic',
      href: null,
      command: 'temps agent ingress delegated',
      connector_id: null,
      hostname: null,
    },
  ],
}

function renderBanner(value: ReachabilityWarning | null) {
  return render(
    <MemoryRouter>
      <ReachabilityWarningBanner warning={value} />
    </MemoryRouter>
  )
}

test('says why nobody can reach the environment, with one action per fix', () => {
  const { container } = renderBanner(warning)
  expect(container.textContent).toContain(warning.headline)
  expect(container.textContent).toContain(warning.reason)
  const links = Array.from(container.querySelectorAll('a')).map((a) => [
    a.textContent,
    a.getAttribute('href'),
  ])
  expect(links).toEqual([
    [warning.fixes[0].label, '/settings/nodes/ingress'],
    [warning.fixes[1].label, '/settings#preview-domain'],
  ])
  // A fix that is a command on a worker is shown to copy, not linked.
  expect(container.querySelector('code')?.textContent).toBe(
    'temps agent ingress delegated'
  )
})

test('renders nothing when the environment is reachable', () => {
  const { container } = renderBanner(null)
  expect(container.textContent).toBe('')
})

test('on a project, the tunnel fix runs in place instead of linking away', () => {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const { container } = render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <ReachabilityWarningBanner warning={warning} projectId={7} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const hrefs = Array.from(container.querySelectorAll('a')).map((a) =>
    a.getAttribute('href')
  )
  expect(hrefs).toEqual(['/settings#preview-domain'])
  const buttons = Array.from(container.querySelectorAll('button')).map(
    (b) => b.textContent
  )
  expect(buttons).toContain(warning.fixes[0].label)
})
