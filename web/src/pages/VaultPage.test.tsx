// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { render as mount, cleanup, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
installBrowserWindow()
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  accessCapabilityOptions,
  listAccessVaultsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import VaultPage from './VaultPage'
import type { AccessVaultList } from '@/api/client'

function render(vaults: AccessVaultList, write = true) {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false } },
  })
  client.setQueryData(
    accessCapabilityOptions({ path: { project_id: 7 } }).queryKey,
    {
      can_manage_services: write,
      can_manage_requests: write,
      can_save_requests: true,
      can_use_project_github: false,
      configured: true,
      state: 'available',
      reason: 'Available',
    }
  )
  client.setQueryData(
    listAccessVaultsOptions({
      path: { project_id: 7 },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    vaults
  )
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/vault/projects/7']}>
        <BreadcrumbProvider>
          <Routes>
            <Route path="/vault/projects/:projectId" element={<VaultPage />} />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}
test('empty vaults teach the next action without fabricated activity or pagination', () => {
  const html = render({ items: [], total: 0, page: 1, page_size: 20 })
  expect(html).toContain('Create your first vault')
  expect(html).not.toContain('Idle')
  expect(html).not.toContain('Pagination')
})
test('vault cards link to named vaults and show their services', () => {
  const html = render({
    items: [
      {
        vault: {
          id: 'vault_test',
          name: 'Development',
          service_ids: ['svc_test'],
          project_id: 7,
          version: 1,
          created_at: '2026-01-01T00:00:00Z',
          updated_at: '2026-01-01T00:00:00Z',
        },
        services: [
          {
            id: 'svc_test',
            name: 'Repository API',
            project_id: 7,
            max_ttl_seconds: 900,
            enabled: true,
            has_credential: true,
            version: 1,
            created_at: '2026-01-01T00:00:00Z',
            updated_at: '2026-01-01T00:00:00Z',
            policy: {
              provider: 'github',
              origin: 'https://api.github.com',
              path_prefixes: ['/'],
              methods: ['GET'],
            },
          },
        ],
      },
    ],
    total: 1,
    page: 1,
    page_size: 20,
  })
  expect(html).toContain('/vault/projects/7/vaults/vault_test')
  expect(html).toContain('Development')
  expect(html).toContain('Repository API')
  expect(html).toContain('alt="GitHub"')
})
test('readers cannot create vaults', () => {
  const html = render({ items: [], total: 0, page: 1, page_size: 20 }, false)
  expect(html).toContain('disabled=""')
  expect(html).toContain('Project write permission is required')
})

test('empty later project pages return to page one', async () => {
  const { getProjectsOptions } =
    await import('@/api/client/@tanstack/react-query.gen')
  const { useLocation } = await import('react-router')
  function Location() {
    const location = useLocation()
    return (
      <output aria-label="Location">
        {location.pathname}
        {location.search}
      </output>
    )
  }
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false } },
  })
  client.setQueryData(
    getProjectsOptions({ query: { page: 2, per_page: 20 } }).queryKey,
    { projects: [], total: 1, page: 2, per_page: 20 }
  )
  client.setQueryData(
    getProjectsOptions({ query: { page: 1, per_page: 20 } }).queryKey,
    { projects: [], total: 0, page: 1, per_page: 20 }
  )
  try {
    const view = mount(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={['/vault?page=2']}>
          <BreadcrumbProvider>
            <Location />
            <VaultPage />
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    await waitFor(() =>
      expect(view.getByLabelText('Location').textContent).toBe('/vault')
    )
  } finally {
    cleanup()
    client.clear()
  }
})
