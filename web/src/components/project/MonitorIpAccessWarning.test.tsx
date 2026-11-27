// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { UptimeMonitorIpBlock } from '@/api/client/types.gen'
import { MonitorIpAccessWarning } from './MonitorIpAccessWarning'

const block: UptimeMonitorIpBlock = {
  project_id: 2,
  environment_id: 7,
  policy_scope: 'project',
  mode: 'allow',
  reason: 'not_allow_listed',
  monitor_addresses: ['127.0.0.1', '203.0.113.10'],
  blocked_addresses: ['127.0.0.1', '203.0.113.10'],
  matched_cidrs: [],
  suggested_cidrs: ['127.0.0.1/32', '203.0.113.10/32'],
  suggested_description: 'Temps uptime monitor',
  message:
    "Blocked by this project's IP access list: Temps' uptime monitor connects from 127.0.0.1 and 203.0.113.10, which the list does not allow.",
}

function render(value: UptimeMonitorIpBlock) {
  const client = new QueryClient()
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <MonitorIpAccessWarning
          block={value}
          monitorId={11}
          projectSlug="web"
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  client.clear()
  return markup
}

test('names the addresses and offers to allow the monitor', () => {
  const markup = render(block)
  expect(markup).toContain('The IP access list blocks this monitor')
  expect(markup).toContain('connects from 127.0.0.1 and 203.0.113.10')
  expect(markup).toContain('Allow monitor')
  expect(markup).toContain(
    'Add 127.0.0.1/32, 203.0.113.10/32 to the list as &quot;Temps uptime monitor&quot;'
  )
})

test('links to the project list when that list refuses the monitor', () => {
  expect(render(block)).toContain(
    'href="/projects/web/settings/security#ip-access"'
  )
})

test("links to the environment's own list when that is the one", () => {
  expect(render({ ...block, policy_scope: 'environment' })).toContain(
    'href="/projects/web/environments/7?view=settings#ip-access"'
  )
})

test('a deny-list block has nothing to add, only the link', () => {
  const markup = render({
    ...block,
    mode: 'deny',
    reason: 'deny_listed',
    matched_cidrs: ['127.0.0.0/8'],
    suggested_cidrs: [],
  })
  expect(markup).not.toContain('Allow monitor')
  expect(markup).toContain('IP access')
})
