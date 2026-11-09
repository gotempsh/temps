// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  accessCapabilityOptions,
  getAccessVaultOptions,
  listAccessBundlesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { AccessServiceResponse, AccessVaultResponse } from '@/api/client'
import VaultDetailPage from './VaultDetailPage'
import AccessBundlePage from './AccessBundlePage'
import AccessBundleDetailPage from './AccessBundleDetailPage'

installBrowserWindow()
afterEach(async () => {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20))
    cleanup()
  })
})
const date = '2026-01-01T00:00:00Z'
const vault: AccessVaultResponse = {
  id: 'vault_test',
  project_id: 7,
  name: 'Development',
  service_ids: ['svc_test'],
  version: 3,
  created_at: date,
  updated_at: date,
}
const service: AccessServiceResponse = {
  id: 'svc_test',
  project_id: 7,
  name: 'Repository API',
  policy: {
    provider: 'github',
    origin: 'https://api.github.com',
    path_prefixes: ['/'],
    methods: ['GET'],
  },
  has_credential: true,
  enabled: true,
  max_ttl_seconds: 1800,
  version: 1,
  created_at: date,
  updated_at: date,
}
function mount(tab = 'services', write = true, entry?: string) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity },
      mutations: { retry: false },
    },
  })
  client.setQueryData(
    accessCapabilityOptions({ path: { project_id: 7 } }).queryKey,
    {
      can_manage_services: write,
      can_manage_requests: write,
      can_save_requests: true,
      can_use_project_github: false,
      reason: 'Available',
      state: 'available',
      configured: true,
    }
  )
  client.setQueryData(
    getAccessVaultOptions({ path: { project_id: 7, vault_id: vault.id } })
      .queryKey,
    { vault, services: [service] }
  )
  client.setQueryData(
    listAccessBundlesOptions({
      path: { project_id: 7, vault_id: vault.id },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    { items: [], total: 0, page: 1, page_size: 20 }
  )
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter
        initialEntries={[
          entry ?? `/vault/projects/7/vaults/vault_test?tab=${tab}`,
        ]}
      >
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/projects/:projectId/access-services/:serviceId/edit"
              element={<h1>Service details destination</h1>}
            />
            <Route
              path="/vault/projects/:projectId/vaults/:vaultId/bundles/:bundleId"
              element={<AccessBundleDetailPage />}
            />
            <Route
              path="/vault/projects/:projectId/vaults/:vaultId/bundles/new"
              element={<AccessBundlePage />}
            />
            <Route
              path="/vault/projects/:projectId/vaults/:vaultId/bundles/:bundleId/edit"
              element={<AccessBundlePage />}
            />
            <Route
              path="/vault/projects/:projectId/vaults/:vaultId"
              element={<VaultDetailPage />}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return client
}
test('session pagination survives switching vault tabs without fetching hidden sessions', async () => {
  const pages: number[] = []
  const api = mockApi({
    'GET /projects/7/access-sessions': (request) => {
      const page = Number(new URL(request.url).searchParams.get('page'))
      pages.push(page)
      return jsonResponse({
        items: [
          {
            id: `session_${page}`,
            bundle_id: 'bundle_test',
            bundle_name: `Session page ${page}`,
            bundle_version: 1,
            consumer: { sandbox_id: 'sandbox_test' },
            created_at: date,
            expires_at: date,
            project_id: 7,
            service_count: 1,
            status: 'expired',
            vault_id: vault.id,
          },
        ],
        total: 21,
        page,
        page_size: 20,
      })
    },
  })
  const selectTab = async (name: string) => {
    await act(async () =>
      fireEvent.mouseDown(
        within(document.body).getByRole('tab', {
          name: new RegExp(`^${name}`),
        }),
        { button: 0, ctrlKey: false }
      )
    )
  }
  const client = mount()
  try {
    expect(pages).toEqual([])
    await selectTab('Sessions')
    await waitFor(() =>
      expect(
        within(document.body).getByRole('link', { name: 'Session page 1' })
      ).toBeTruthy()
    )
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', {
          name: 'Next page',
        })
      )
    )
    await waitFor(() =>
      expect(
        within(document.body).getByRole('link', { name: 'Session page 2' })
      ).toBeTruthy()
    )
    for (const tab of ['Services', 'Access bundles']) {
      await selectTab(tab)
      expect(
        within(document.body).queryByRole('table', { name: 'Vault sessions' })
      ).toBeNull()
      const before = pages.length
      await act(async () => {
        await client.invalidateQueries({
          queryKey: [{ _id: 'listAccessSessions' }],
        })
      })
      expect(pages.length).toBe(before)
      await selectTab('Sessions')
      await waitFor(() =>
        expect(
          within(document.body).getByRole('link', { name: 'Session page 2' })
        ).toBeTruthy()
      )
      expect(within(document.body).getByLabelText('Page 2 of 2')).toBeTruthy()
      expect(pages[pages.length - 1]).toBe(2)
    }
  } finally {
    await act(async () => cleanup())
    client.clear()
    api.restore()
  }
})

test('vault services show provider logos and readers have no add-service navigation', () => {
  mount('services', false)
  expect(within(document.body).getAllByAltText('GitHub')).toBeTruthy()
  const add = within(document.body).getByRole('button', {
    name: 'Add service',
  }) as HTMLButtonElement
  expect(add.disabled).toBe(true)
  expect(
    within(document.body).queryByRole('link', { name: 'Add service' })
  ).toBeNull()
  expect(
    (
      within(document.body).getByRole('button', {
        name: 'Edit vault',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)
})
test('bundle save selects services and lifetime without creating a grant', async () => {
  let submitted: unknown
  const api = mockApi({
    'GET /projects/7/access-vaults/vault_test': () =>
      jsonResponse({ vault, services: [service] }),
    'POST /projects/7/access-vaults/vault_test/bundles': async (request) => {
      submitted = await request.json()
      return jsonResponse({ id: 'bundle_test' })
    },
    'GET /projects/7/access-vaults/vault_test/bundles': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
  })
  try {
    mount('bundles')
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', {
          name: 'Create access bundle',
        })
      )
    )
    await waitFor(() =>
      expect(
        within(document.body).getByRole('heading', {
          name: 'Create access bundle',
        })
      ).toBeTruthy()
    )
    await act(async () =>
      fireEvent.change(within(document.body).getByLabelText('Bundle name'), {
        target: { value: 'Read repository' },
      })
    )
    await act(async () =>
      fireEvent.click(within(document.body).getByRole('checkbox'))
    )
    await act(async () =>
      fireEvent.change(
        within(document.body).getByLabelText('Access lifetime (seconds)'),
        { target: { value: '1800' } }
      )
    )
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', {
          name: 'Save access bundle',
        })
      )
    )
    await waitFor(() =>
      expect(submitted).toEqual({
        name: 'Read repository',
        service_ids: ['svc_test'],
        ttl_seconds: 1800,
      })
    )
  } finally {
    api.restore()
  }
})
test('failed bundle save and a vault refetch retain the draft', async () => {
  const api = mockApi({
    'POST /projects/7/access-vaults/vault_test/bundles': () =>
      jsonResponse({ detail: 'Lifetime exceeds the service limit' }, 400),
    'GET /projects/7/access-vaults/vault_test': () =>
      jsonResponse({ vault: { ...vault, version: 4 }, services: [service] }),
  })
  try {
    const client = mount('bundles')
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', {
          name: 'Create access bundle',
        })
      )
    )
    await waitFor(() =>
      expect(
        within(document.body).getByRole('heading', {
          name: 'Create access bundle',
        })
      ).toBeTruthy()
    )
    await act(async () =>
      fireEvent.change(within(document.body).getByLabelText('Bundle name'), {
        target: { value: 'Keep my draft' },
      })
    )
    await act(async () =>
      fireEvent.click(within(document.body).getByRole('checkbox'))
    )
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', {
          name: 'Save access bundle',
        })
      )
    )
    await waitFor(() =>
      expect(
        within(document.body).getByText('Could not save access bundle')
      ).toBeTruthy()
    )
    await act(async () => {
      await client.invalidateQueries({ queryKey: [{ _id: 'getAccessVault' }] })
      await new Promise((resolve) => setTimeout(resolve, 20))
    })
    expect(
      (within(document.body).getByLabelText('Bundle name') as HTMLInputElement)
        .value
    ).toBe('Keep my draft')
    expect(
      within(document.body).getByRole('checkbox').getAttribute('data-state')
    ).toBe('checked')
  } finally {
    api.restore()
  }
})

test('a failed bundle refresh cannot pin an obsolete cached edit', async () => {
  const api = mockApi({
    'GET /projects/7/access-vaults/vault_test/bundles': () =>
      jsonResponse({ detail: 'Temporarily unavailable' }, 503),
  })
  try {
    const client = mount('bundles')
    await act(async () => {
      client.setQueryData(
        listAccessBundlesOptions({
          path: { project_id: 7, vault_id: vault.id },
          query: { page: 1, page_size: 20 },
        }).queryKey,
        {
          items: [
            {
              id: 'bundle_test',
              project_id: 7,
              vault_id: vault.id,
              name: 'Cached bundle',
              service_ids: [service.id],
              ttl_seconds: 900,
              version: 1,
              created_at: date,
              updated_at: date,
            },
          ],
          total: 1,
          page: 1,
          page_size: 20,
        }
      )
      await client.invalidateQueries({
        queryKey: [{ _id: 'listAccessBundles' }],
      })
      await new Promise((resolve) => setTimeout(resolve, 20))
    })
    await act(async () =>
      fireEvent.pointerDown(
        within(document.body).getByRole('button', {
          name: 'Actions for Cached bundle',
        }),
        { button: 0, ctrlKey: false }
      )
    )
    const edit = within(document.body).getByRole('menuitem', {
      name: 'Edit bundle',
    })
    expect(edit.getAttribute('aria-disabled')).toBe('true')
    expect(
      within(document.body).queryByRole('textbox', { name: 'Bundle name' })
    ).toBeNull()
  } finally {
    api.restore()
  }
})

test('direct bundle edit loads a fresh version and saves back to the list', async () => {
  let submitted: unknown
  const existing = {
    id: 'bundle_test',
    project_id: 7,
    vault_id: vault.id,
    name: 'Repository access',
    service_ids: [service.id],
    ttl_seconds: 900,
    version: 5,
    created_at: date,
    updated_at: date,
  }
  const api = mockApi({
    'GET /projects/7/access-vaults/vault_test': () =>
      jsonResponse({ vault, services: [service] }),
    'GET /projects/7/access-vaults/vault_test/bundles': () =>
      jsonResponse({ items: [existing], total: 1, page: 1, page_size: 100 }),
    'PUT /projects/7/access-vaults/vault_test/bundles/bundle_test': async (
      request
    ) => {
      submitted = await request.json()
      return jsonResponse({ ...existing, version: 6 })
    },
  })
  try {
    mount(
      'bundles',
      true,
      '/vault/projects/7/vaults/vault_test/bundles/bundle_test/edit'
    )
    await waitFor(() =>
      expect(
        within(document.body).getByRole('heading', {
          name: 'Edit access bundle',
        })
      ).toBeTruthy()
    )
    expect(within(document.body).queryByRole('tablist')).toBeNull()
    await act(async () =>
      fireEvent.change(within(document.body).getByLabelText('Bundle name'), {
        target: { value: 'Updated access' },
      })
    )
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', {
          name: 'Save access bundle',
        })
      )
    )
    await waitFor(() =>
      expect(submitted).toEqual({
        name: 'Updated access',
        service_ids: [service.id],
        ttl_seconds: 900,
        expected_version: 5,
      })
    )
    await waitFor(() =>
      expect(within(document.body).getByRole('tablist')).toBeTruthy()
    )
    expect(
      within(document.body).queryByRole('textbox', { name: 'Bundle name' })
    ).toBeNull()
  } finally {
    api.restore()
  }
})

test('bundle card opens read-only details and vault has no sandbox grant tab', async () => {
  const existing = {
    id: 'bundle_test',
    project_id: 7,
    vault_id: vault.id,
    name: 'Repository access',
    service_ids: [service.id],
    ttl_seconds: 900,
    version: 2,
    created_at: date,
    updated_at: date,
  }
  const api = mockApi({
    'GET /projects/7/access-vaults/vault_test/bundles': () =>
      jsonResponse({ items: [existing], total: 1, page: 1, page_size: 100 }),
  })
  try {
    const client = mount('bundles', false)
    await act(async () => {
      client.setQueryData(
        listAccessBundlesOptions({
          path: { project_id: 7, vault_id: vault.id },
          query: { page: 1, page_size: 20 },
        }).queryKey,
        { items: [existing], total: 1, page: 1, page_size: 20 }
      )
      await new Promise((resolve) => setTimeout(resolve, 20))
    })
    expect(
      within(document.body).queryByRole('tab', { name: 'Sandbox access' })
    ).toBeNull()
    const card = within(document.body).getByRole('link', {
      name: 'Repository access',
    })
    expect(card.getAttribute('href')).toBe(
      '/vault/projects/7/vaults/vault_test/bundles/bundle_test'
    )
    await act(async () => fireEvent.click(card))
    await waitFor(() =>
      expect(
        within(document.body).getByRole('heading', {
          name: 'Repository access',
        })
      ).toBeTruthy()
    )
    expect(within(document.body).getByText('15 min')).toBeTruthy()
    expect(within(document.body).getByText('Repository API')).toBeTruthy()
    expect(
      (
        within(document.body).getByRole('button', {
          name: 'Edit bundle',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(true)
    expect(
      within(document.body).queryByRole('button', { name: 'Grant access' })
    ).toBeNull()
  } finally {
    api.restore()
  }
})

test('vault draft survives dismissal and refetch, and successful saves advance its version', async () => {
  let saved = vault
  const submissions: Array<{
    name: string
    service_ids: string[]
    expected_version: number
  }> = []
  const api = mockApi({
    'GET /projects/7/access-services': () =>
      jsonResponse({ items: [service], total: 1, page: 1, page_size: 20 }),
    'GET /projects/7/access-vaults/vault_test': () =>
      jsonResponse({ vault: saved, services: [service] }),
    'PUT /projects/7/access-vaults/vault_test': async (request) => {
      const body = (await request.json()) as (typeof submissions)[number]
      submissions.push(body)
      saved = {
        ...saved,
        name: body.name,
        service_ids: body.service_ids,
        version: saved.version + 1,
      }
      return jsonResponse(saved)
    },
  })
  let client: QueryClient | undefined
  const open = async () => {
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Edit vault' })
      )
    )
    await waitFor(() =>
      expect(within(document.body).getByRole('checkbox')).toBeTruthy()
    )
  }
  try {
    client = mount()
    await open()
    await act(async () => {
      fireEvent.change(within(document.body).getByLabelText('Vault name'), {
        target: { value: 'Staging draft' },
      })
      fireEvent.click(within(document.body).getByRole('checkbox'))
    })
    await act(async () =>
      fireEvent.keyDown(within(document.body).getByRole('dialog'), {
        key: 'Escape',
        code: 'Escape',
        keyCode: 27,
      })
    )
    await waitFor(() =>
      expect(within(document.body).queryByRole('dialog')).toBeNull()
    )
    await act(async () => {
      client!.setQueryData(
        getAccessVaultOptions({ path: { project_id: 7, vault_id: vault.id } })
          .queryKey,
        {
          vault: { ...vault, name: 'Refetched name', version: 9 },
          services: [service],
        }
      )
    })
    await open()
    expect(
      (within(document.body).getByLabelText('Vault name') as HTMLInputElement)
        .value
    ).toBe('Staging draft')
    expect(
      within(document.body).getByRole('checkbox').getAttribute('data-state')
    ).toBe('unchecked')
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Save vault' })
      )
    )
    await waitFor(() =>
      expect(within(document.body).queryByRole('dialog')).toBeNull()
    )
    expect(submissions[0]).toEqual({
      name: 'Staging draft',
      service_ids: [],
      expected_version: 3,
    })
    await open()
    expect(
      (within(document.body).getByLabelText('Vault name') as HTMLInputElement)
        .value
    ).toBe('Staging draft')
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Save vault' })
      )
    )
    await waitFor(() => expect(submissions.length).toBe(2))
    expect(submissions[1].expected_version).toBe(4)
  } finally {
    await act(async () => cleanup())
    client?.clear()
    api.restore()
  }
})

test('clicking service row outside its name opens the service', async () => {
  const client = mount()
  try {
    const table = within(document.body).getByRole('table', {
      name: 'Vault services',
    })
    const link = within(table).getByRole('link', { name: 'Repository API' })
    expect(link.getAttribute('href')).toBe(
      '/projects/7/access-services/svc_test/edit?vault=vault_test'
    )
    await act(async () => fireEvent.click(within(table).getByText('Allowed')))
    await waitFor(() =>
      expect(
        within(document.body).getByRole('heading', {
          name: 'Service details destination',
        })
      ).toBeTruthy()
    )
  } finally {
    await act(async () => cleanup())
    client.clear()
  }
})
