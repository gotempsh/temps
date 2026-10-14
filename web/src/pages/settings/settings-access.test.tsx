// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ReactElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AuthContext } from '@/contexts/AuthContext-shared'
import {
  getAdminGateOptions,
  nodeMetricsGetAlertRulesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { AgentSandboxSandboxPage } from '@/pages/agent-sandbox/AgentSandboxSandboxPage'
import DeliveryProfiles from '@/pages/DeliveryProfiles'
import { Settings } from '@/pages/Settings'
import { NodeAlertRules } from '@/components/monitoring/NodeAlertRules'
import { AdminGateCard } from '@/components/settings/AdminGateCard'
import { McpServerCard } from '@/components/settings/McpServerCard'
import { BuildLimitsPage } from './BuildLimitsPage'
import { DiskMonitoringPage } from './DiskMonitoringPage'
import { DockerRegistryPage } from './DockerRegistryPage'
import { MonitoringSettingsPage } from './MonitoringSettingsPage'
import { RateLimitingPage } from './RateLimitingPage'
import { RequestTimeoutsPage } from './RequestTimeoutsPage'
import { SecurityPage } from './SecurityPage'

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires settings:read permission',
}

function client() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function fail(queryClient: QueryClient, queryKey: QueryKey, error: unknown) {
  queryClient
    .getQueryCache()
    .build(queryClient, { queryKey })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}

function renderAs(role: string, queryClient: QueryClient, page: ReactElement) {
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Example User',
              username: 'example',
              avatar_url: '',
              mfa_enabled: false,
              role,
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>{page}</BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const SETTINGS_PAGES: [string, ReactElement][] = [
  ['Platform', <Settings key="platform" />],
  ['Security', <SecurityPage key="security" />],
  ['Rate limiting', <RateLimitingPage key="rate" />],
  ['Build limits', <BuildLimitsPage key="build" />],
  ['Request timeouts', <RequestTimeoutsPage key="timeouts" />],
  ['Docker registry', <DockerRegistryPage key="registry" />],
  ['Disk monitoring', <DiskMonitoringPage key="disk" />],
  ['Metrics monitoring', <MonitoringSettingsPage key="metrics" />],
  ['Sandbox configuration', <AgentSandboxSandboxPage key="sandbox" />],
]

describe('settings pages for a role that cannot read platform settings', () => {
  for (const [name, page] of SETTINGS_PAGES) {
    test(`${name} says access is denied instead of showing built-in defaults`, () => {
      const queryClient = client()
      const html = renderAs('user', queryClient, page)
      expect(html).toContain('Platform settings: access denied')
      expect(html).toContain('settings:read')
      // No form, no Save, no Retry: nothing here can be acted on.
      expect(html).not.toContain('<form')
      expect(html).not.toContain('Retry')
      queryClient.clear()
    })
  }

  test('an administrator whose read failed still gets a Retry', () => {
    const queryClient = client()
    fail(queryClient, ['platform-settings'], {
      title: 'Internal Server Error',
      status: 500,
    })
    const html = renderAs('admin', queryClient, <BuildLimitsPage />)
    expect(html).toContain('Platform settings unavailable')
    expect(html).toContain('Retry')
    queryClient.clear()
  })
})

describe('other cards that depend on a read the role lacks', () => {
  test('MCP server settings name the permission instead of a server error', () => {
    const queryClient = client()
    const html = renderAs('user', queryClient, <McpServerCard />)
    expect(html).toContain('MCP server settings: access denied')
    expect(html).not.toContain('Check console logs')
    queryClient.clear()
  })

  test('a refused admin gate read is explained with the server detail', () => {
    const queryClient = client()
    fail(queryClient, getAdminGateOptions().queryKey, refused)
    const html = renderAs('user', queryClient, <AdminGateCard />)
    expect(html).toContain('Admin access gate: access denied')
    expect(html).toContain('Requires settings:read permission')
    expect(html).not.toContain('Check console logs')
    queryClient.clear()
  })

  test('a refused alert rules read offers no Retry', () => {
    const queryClient = client()
    fail(
      queryClient,
      nodeMetricsGetAlertRulesOptions({ path: { id: 0 } }).queryKey,
      refused
    )
    const html = renderAs('user', queryClient, <NodeAlertRules />)
    expect(html).toContain('Platform alert rules: access denied')
    expect(html).not.toContain('Retry')
    queryClient.clear()
  })

  test('delivery profiles explain the settings gap at once and do not claim a missing profile', () => {
    const queryClient = client()
    const html = renderAs('user', queryClient, <DeliveryProfiles />)
    expect(html).toContain('Instance settings access is required')
    expect(html).not.toContain('Create a Cloudflare delivery profile')
    queryClient.clear()
  })
})
