// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { listSandboxInternalAddressesOptions } from '@/api/client/@tanstack/react-query.gen'
import type { SandboxInternalAddressListResponse } from '@/api/client/types.gen'
import {
  INTERNAL_ADDRESSES_PAGE_SIZE,
  SandboxInternalAddressesCard,
  internalAccessSettingsPath,
} from './SandboxInternalAddressesCard'

function render(data?: SandboxInternalAddressListResponse, disabled = false) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
  if (data) {
    client.setQueryData(
      listSandboxInternalAddressesOptions({
        path: { sandbox_id: 'sbx_alpha' },
        query: { page: 1, page_size: INTERNAL_ADDRESSES_PAGE_SIZE },
      }).queryKey,
      data
    )
  }
  const markup = renderToStaticMarkup(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <SandboxInternalAddressesCard
          sandboxId="sbx_alpha"
          disabled={disabled}
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
  client.clear()
  return markup
}

test('each address shows the exact curl lines to run in the sandbox, by both names', () => {
  const markup = render({
    items: [
      {
        grant_id: 41,
        project_id: 1,
        project_name: 'Orders API',
        project_slug: 'orders-api',
        environment_id: 3,
        environment_name: 'production',
        hostname: 'production.orders-api.temps.local',
        url: 'http://production.orders-api.temps.local/',
        curl_command: 'curl -sS http://production.orders-api.temps.local/',
        alias_hostname: 'orders-api-production.temps.local',
        alias_curl_command:
          'curl -sS http://orders-api-production.temps.local/',
      },
      {
        // A managed service holds this environment's subdomain alias.
        grant_id: 41,
        project_id: 1,
        project_name: 'Orders API',
        project_slug: 'orders-api',
        environment_id: 4,
        environment_name: 'staging',
        hostname: 'staging.orders-api.temps.local',
        url: 'http://staging.orders-api.temps.local/',
        curl_command: 'curl -sS http://staging.orders-api.temps.local/',
        alias_hostname: null,
        alias_curl_command: null,
      },
    ],
    total: 1,
    page: 1,
    page_size: INTERNAL_ADDRESSES_PAGE_SIZE,
  })
  expect(markup).toContain('Internal addresses this sandbox can call')
  expect(markup).toContain('curl -sS http://production.orders-api.temps.local/')
  expect(markup).toContain(
    'Copy the curl command for production.orders-api.temps.local'
  )
  expect(markup).toContain('curl -sS http://orders-api-production.temps.local/')
  expect(markup).toContain(
    'Copy the curl command for orders-api-production.temps.local'
  )
  expect(markup).toContain('curl -sS http://staging.orders-api.temps.local/')
  // Only the environment that has its alias offers it.
  expect(
    markup.split('Or by the environment&#x27;s subdomain').length - 1
  ).toBe(1)
  expect(markup).toContain(
    'href="/projects/orders-api/settings/security#internal-access"'
  )
})

test('without grants it says the call gets a 403 and lists projects to allow', () => {
  const markup = render({
    items: [],
    total: 0,
    page: 1,
    page_size: INTERNAL_ADDRESSES_PAGE_SIZE,
  })
  expect(markup).toContain('No project lets this sandbox call it yet')
  expect(markup).toContain('gets a 403. Allow a project below.')
  // The reverse direction: projects, searchable, one click each.
  expect(markup).toContain('aria-label="Search projects"')
})

test('a destroyed sandbox says it calls nothing, and offers no projects', () => {
  const markup = render(undefined, true)
  expect(markup).toContain('A destroyed sandbox calls nothing')
  expect(markup).not.toContain('aria-label="Search projects"')
})

test('the settings path is the project Protection page anchor', () => {
  expect(internalAccessSettingsPath('shop')).toBe(
    '/projects/shop/settings/security#internal-access'
  )
})
