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
