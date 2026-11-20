// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import {
  dnsSummary,
  type ClusterDnsResolversResponse,
} from '@/lib/dns-observability'
import {
  CONSOLE,
  ConsoleShell,
  expectOnlyConsoleScrolled,
} from '@/test/console-shell'
import { installBrowserWindow } from '@/test/dom'
import { installFakeScrollLayout } from '@/test/fake-scroll-layout'
import { mockApi } from '@/test/fetch-mock'
import { DnsOverview } from './DnsOverview'

installBrowserWindow()
const layout = installFakeScrollLayout()
afterEach(cleanup)

const data: ClusterDnsResolversResponse = {
  cluster_dns_enabled: true,
  zone_record_count: 3,
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
      record_count: 3,
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
      health_reason: 'The resolver is not running',
      running: false,
      tasks_alive: false,
      consecutive_sync_failures: 3,
      dropped_log_entries: 0,
    },
  ],
  open_alerts: [],
}

function renderOverview(search: string) {
  mockApi({})
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <ConsoleShell>
          <DnsOverview
            data={data}
            summary={dnsSummary(data.resolvers, true)}
            search={new URLSearchParams(search)}
          />
        </ConsoleShell>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an opened node below the view is scrolled up just into the page, not the shell', () => {
  layout.place('#dns-node-7', { top: 1500, height: 120 })
  renderOverview('node=7')
  // Its bottom at the bottom of the page's view: 1500 + 120 - 700.
  expectOnlyConsoleScrolled(layout, 920)
})

test('an opened node taller than the view shows its top and leaves the shell in place', () => {
  // On a phone the details outgrow the screen: `scrollIntoView` lined the
  // row up in the page, then dragged the shell along to fit the rest.
  layout.place('#dns-node-7', { top: 1000, height: CONSOLE.height + 200 })
  renderOverview('node=7')
  expectOnlyConsoleScrolled(layout, 1000)
})

test('an opened node already in view is left where it is', () => {
  layout.place('#dns-node-7', { top: 300, height: 120 })
  renderOverview('node=7')
  expect(layout.scrolls).toEqual([])
  expect(layout.intoView).toEqual([])
})
