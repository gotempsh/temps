// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { UserResponse } from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  networkPolicyKeys,
  type ListRulesParams,
  type NetworkPolicyRule,
  type NodeNetworkPolicyModeList,
} from '@/lib/network-policy'
import { NetworkPolicyPage } from './NetworkPolicyPage'

const firstPage: ListRulesParams = {
  page: 1,
  page_size: 20,
  origin: undefined,
  subject_kind: undefined,
  subject_id: undefined,
}

function rule(overrides: Partial<NetworkPolicyRule>): NetworkPolicyRule {
  return {
    id: 1,
    action: 'allow',
    direction: 'egress',
    enforceable: true,
    origin: 'operator',
    peer_kind: 'cidr',
    peer_ref: '10.0.0.0/8',
    protocol: 'tcp',
    port_start: 5432,
    port_end: 5432,
    subject_kind: 'sandbox',
    subject_id: 42,
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...overrides,
  }
}

const nodes: NodeNetworkPolicyModeList = {
  nodes: [
    {
      node_id: 0,
      name: 'control-plane',
      mode: 'enforce',
      role: 'control_plane',
      status: 'active',
      supports_enforcement: true,
    },
  ],
}

function createClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function fail(client: QueryClient, queryKey: QueryKey, error: unknown) {
  client
    .getQueryCache()
    .build(client, { queryKey })
    .setState({ status: 'error', error: error as Error, fetchStatus: 'idle' })
}

function render(client: QueryClient, role: string | null, path = '/') {
  const user = role ? ({ id: 1, role } as unknown as UserResponse) : null
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <AuthContext.Provider
        value={{
          user,
          isLoading: false,
          error: null,
          logout: async () => {},
          refetch: () => {},
        }}
      >
        <MemoryRouter initialEntries={[path]}>
          <BreadcrumbProvider>
            <NetworkPolicyPage />
          </BreadcrumbProvider>
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

describe('NetworkPolicyPage', () => {
  test('lists rules in a table with enforcement and link guards', () => {
    const client = createClient()
    client.setQueryData(networkPolicyKeys.nodes, nodes)
    client.setQueryData(networkPolicyKeys.rules(firstPage), {
      items: [
        rule({ id: 7 }),
        rule({
          id: 8,
          origin: 'link',
          link_id: 3,
          link_table: 'sandbox_links',
          peer_kind: 'host',
          peer_ref: 'api.example.test',
          enforceable: false,
        }),
      ],
      total: 2,
      page: 1,
      page_size: 20,
    })
    const html = render(client, 'admin')

    expect(html).toContain('Network policy')
    expect(html).toContain('Where it applies')
    expect(html).toContain('control-plane')
    expect(html).toContain('#7')
    expect(html).toContain('Delete rule #7')
    // A link rule is removed by unlinking, so it offers no delete control.
    expect(html).not.toContain('Delete rule #8')
    expect(html).toContain('Managed by a link')
    expect(html).toContain('Add rule')
    expect(html).toContain('Network policy rules pagination')
    // SettingsLayout owns the page container; the page must not add one.
    expect(html).not.toContain('data-page-container')
    client.clear()
  })

  test('a non-admin sees the rules without add or delete controls', () => {
    const client = createClient()
    client.setQueryData(networkPolicyKeys.nodes, nodes)
    client.setQueryData(networkPolicyKeys.rules(firstPage), {
      items: [rule({ id: 7 })],
      total: 1,
      page: 1,
      page_size: 20,
    })
    const html = render(client, 'user')

    expect(html).toContain('#7')
    expect(html).not.toContain('Delete rule #7')
    expect(html).not.toContain('Add rule')
    client.clear()
  })

  test('an empty, unfiltered list explains what rules do and offers to add one', () => {
    const client = createClient()
    client.setQueryData(networkPolicyKeys.nodes, nodes)
    client.setQueryData(networkPolicyKeys.rules(firstPage), {
      items: [],
      total: 0,
      page: 1,
      page_size: 20,
    })
    const html = render(client, 'admin')

    expect(html).toContain('No network policy rules yet')
    expect(html).toContain('sandboxes reach only what a link allows')
    expect(html).toContain('Add rule')
    expect(html).not.toContain('Network policy rules pagination')
    client.clear()
  })

  test('a filter with no match offers to clear the filters', () => {
    const client = createClient()
    client.setQueryData(networkPolicyKeys.nodes, nodes)
    client.setQueryData(
      networkPolicyKeys.rules({ ...firstPage, origin: 'system' }),
      { items: [], total: 0, page: 1, page_size: 20 }
    )
    const html = render(client, 'admin', '/?origin=system')

    expect(html).toContain('No matching rules')
    expect(html).toContain('Clear filters')
    client.clear()
  })

  test('a refused rule read keeps the page and explains the refusal', () => {
    const client = createClient()
    client.setQueryData(networkPolicyKeys.nodes, nodes)
    fail(client, networkPolicyKeys.rules(firstPage), {
      title: 'Forbidden',
      status: 403,
      detail: 'Requires settings:read permission',
    })
    const html = render(client, 'user')

    expect(html).toContain('Network policy')
    expect(html).toContain('Requires settings:read permission')
    expect(html).not.toContain('No network policy rules yet')
    client.clear()
  })
})
