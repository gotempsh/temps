// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import {
  render as mount,
  cleanup,
  waitFor,
  act,
  fireEvent,
  within,
} from '@testing-library/react'
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
import { jsonResponse, mockApi } from '@/test/fetch-mock'

test('vault creation freezes the name while pending and preserves it after failure', async () => {
  const names: string[] = []
  let finish: ((response: Response) => void) | undefined
  const api = mockApi({
    'GET /projects/7': () => jsonResponse({ id: 7, name: 'Example project' }),
    'GET /projects/7/access-vaults': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
    'GET /projects/7/service-access/capability': () =>
      jsonResponse({ can_manage_services: true }),
    'POST /projects/7/access-vaults': async (request) => {
      names.push((await request.json()).name)
      return new Promise<Response>((resolve) => {
        finish = resolve
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    const view = mount(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={['/vault/projects/7']}>
          <BreadcrumbProvider>
            <Routes>
              <Route
                path="/vault/projects/:projectId"
                element={<VaultPage />}
              />
              <Route
                path="/vault/projects/:projectId/vaults/:vaultId"
                element={<h1>Created vault</h1>}
              />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    const open = within(view.container).getByRole('button', {
      name: 'Create vault',
    }) as HTMLButtonElement
    await waitFor(() => expect(open.disabled).toBe(false))
    await act(async () => {
      fireEvent.click(open)
    })
    const dialog = within(document.body).getByRole('dialog')
    const name = within(dialog).getByRole('textbox', {
      name: 'Name',
    }) as HTMLInputElement
    await act(async () => {
      fireEvent.change(name, { target: { value: 'First name' } })
    })
    await act(async () => {
      fireEvent.click(
        within(dialog).getByRole('button', { name: 'Create vault' })
      )
    })
    await waitFor(() => expect(names).toEqual(['First name']))
    expect(name.disabled).toBe(true)
    expect(
      (
        within(dialog).getByRole('button', {
          name: 'Cancel',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    await act(async () => {
      finish?.(
        jsonResponse({ detail: 'Creation temporarily unavailable' }, 503)
      )
    })
    await waitFor(() => expect(name.disabled).toBe(false))
    expect(name.value).toBe('First name')
    expect(
      within(dialog).getByText('Creation temporarily unavailable')
    ).toBeTruthy()
    await act(async () => {
      fireEvent.change(name, { target: { value: 'Revised name' } })
    })
    await act(async () => {
      fireEvent.click(
        within(dialog).getByRole('button', { name: 'Create vault' })
      )
    })
    await waitFor(() => expect(names).toEqual(['First name', 'Revised name']))
    expect(name.disabled).toBe(true)
    await act(async () => {
      finish?.(jsonResponse({ id: 'vault_example', name: 'Revised name' }))
    })
    await waitFor(() =>
      expect(
        within(view.container).getByRole('heading', { name: 'Created vault' })
      ).toBeTruthy()
    )
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})

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
