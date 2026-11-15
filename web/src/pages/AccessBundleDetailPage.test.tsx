// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { cleanup, render, waitFor, within } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  BreadcrumbProvider,
  useBreadcrumbs,
} from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import AccessBundleDetailPage from './AccessBundleDetailPage'

installBrowserWindow()

function BreadcrumbLinks() {
  const { breadcrumbs } = useBreadcrumbs()
  return (
    <nav aria-label="Breadcrumbs">
      {breadcrumbs
        .filter((item) => item.href)
        .map((item, index) => (
          <a key={index} href={item.href}>
            {item.label}
          </a>
        ))}
    </nav>
  )
}

test('sandbox access context survives bundle navigation and session creation', async () => {
  const consumers: (string | null)[] = []
  const api = mockApi({
    'GET /service-access/capability': () =>
      jsonResponse({ can_manage_requests: true, can_manage_services: false }),
    'GET /access-vaults/vault_fixture': (request) => {
      consumers.push(new URL(request.url).searchParams.get('sandbox_id'))
      return jsonResponse({
        vault: { id: 'vault_fixture', name: 'Shared tools', service_ids: [] },
        services: [],
      })
    },
    'GET /access-vaults/vault_fixture/bundles': (request) => {
      consumers.push(new URL(request.url).searchParams.get('sandbox_id'))
      return jsonResponse({
        items: [
          {
            id: 'bundle_fixture',
            name: 'Read tools',
            service_ids: [],
            ttl_seconds: 300,
            version: 1,
          },
        ],
        total: 1,
        page: 1,
        page_size: 100,
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_fixture/bundles/bundle_fixture?sandbox=sbx_fixture',
          ]}
        >
          <BreadcrumbProvider>
            <BreadcrumbLinks />
            <Routes>
              <Route
                path="/vault/vaults/:vaultId/bundles/:bundleId"
                element={<AccessBundleDetailPage />}
              />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() =>
      expect(ui.getByRole('heading', { name: 'Read tools' })).toBeTruthy()
    )
    const crumbs = within(ui.getByRole('navigation', { name: 'Breadcrumbs' }))
    await waitFor(() =>
      expect(
        crumbs.getByRole('link', { name: 'Shared tools' }).getAttribute('href')
      ).toBe('/vault/vaults/vault_fixture?sandbox=sbx_fixture')
    )
    expect(
      crumbs.getByRole('link', { name: 'Access bundles' }).getAttribute('href')
    ).toBe('/vault/vaults/vault_fixture?tab=bundles&sandbox=sbx_fixture')
    expect(
      ui.getByRole('link', { name: 'Create session' }).getAttribute('href')
    ).toBe(
      '/vault/vaults/vault_fixture/bundles/bundle_fixture/sessions/new?sandbox=sbx_fixture'
    )
    expect(consumers).toEqual(['sbx_fixture', 'sbx_fixture'])
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

function renderBundle(
  services: unknown[],
  ttlSeconds: number,
  serviceIds: string[]
) {
  const api = mockApi({
    'GET /service-access/capability': () =>
      jsonResponse({ can_manage_requests: true, can_manage_services: false }),
    'GET /access-vaults/vault_fixture': () =>
      jsonResponse({
        vault: { id: 'vault_fixture', name: 'Shared tools', service_ids: [] },
        services,
      }),
    'GET /access-vaults/vault_fixture/bundles': () =>
      jsonResponse({
        items: [
          {
            id: 'bundle_fixture',
            name: 'Read tools',
            service_ids: serviceIds,
            ttl_seconds: ttlSeconds,
            version: 1,
          },
        ],
        total: 1,
        page: 1,
        page_size: 100,
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter
        initialEntries={['/vault/vaults/vault_fixture/bundles/bundle_fixture']}
      >
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/vault/vaults/:vaultId/bundles/:bundleId"
              element={<AccessBundleDetailPage />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return {
    restore: () => {
      cleanup()
      client.clear()
      api.restore()
    },
  }
}

const policy = {
  origin: 'https://api.example.test',
  methods: ['GET'],
  path_prefixes: ['/'],
}
const service = (id: string, enabled: boolean, has_credential: boolean) => ({
  id,
  name: `Service ${id}`,
  provider: 'custom',
  policy,
  enabled,
  has_credential,
  version: 1,
  max_ttl_seconds: 900,
})

test('the verdict and each row say which services cannot be used', async () => {
  const view = renderBundle(
    [
      service('svc_ok', true, true),
      service('svc_off', false, true),
      service('svc_nocred', true, false),
    ],
    90,
    ['svc_ok', 'svc_off', 'svc_nocred']
  )
  try {
    const ui = within(document.body)
    await waitFor(() =>
      expect(ui.getByText("2 services can't be used")).toBeTruthy()
    )
    expect(ui.getByText('Ready')).toBeTruthy()
    expect(ui.getByText('Disabled')).toBeTruthy()
    expect(ui.getByText('No credential')).toBeTruthy()
    // 90 seconds is not a whole number of minutes, so it is not rounded.
    expect(ui.getByText('90 s')).toBeTruthy()
  } finally {
    view.restore()
  }
})

test('a bundle whose services are all usable is ready for sessions', async () => {
  const view = renderBundle([service('svc_ok', true, true)], 900, ['svc_ok'])
  try {
    const ui = within(document.body)
    await waitFor(() => expect(ui.getByText('Ready for sessions')).toBeTruthy())
    expect(ui.getByText('15 min')).toBeTruthy()
    expect(ui.queryByText(/grants no access/)).toBeNull()
  } finally {
    view.restore()
  }
})
