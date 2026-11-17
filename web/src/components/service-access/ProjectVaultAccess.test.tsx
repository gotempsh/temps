// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { projectAccessVaultsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectVaultAccess } from './ProjectVaultAccess'

import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { mockApi, jsonResponse } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
installBrowserWindow()

test('project linked vaults are read-only and point to sandbox consent', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity } },
  })
  client.setQueryData(
    projectAccessVaultsOptions({
      path: { project_id: 7 },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    {
      items: [
        {
          services: [],
          vault: {
            id: 'vault_fixture',
            version: 1,
            created_at: '2026-01-01T00:00:00Z',
            updated_at: '2026-01-01T00:00:00Z',
            name: 'Shared APIs',
            service_ids: ['svc_fixture'],
          },
        },
      ],
      total: 1,
      page: 1,
      page_size: 20,
    }
  )
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ProjectVaultAccess projectId={7} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(html).toContain('Shared APIs')
  expect(html).toContain('Grant access from a sandbox or AI chat')
  expect(html).not.toContain('Unlink')
  expect(html).not.toContain('Edit')
})

test('loading displays skeleton rows before consumer data arrives', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <ProjectVaultAccess projectId={7} />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).toContain('aria-label="Loading linked vaults"')
    expect(html).toContain('animate-pulse')
  } finally {
    client.clear()
  }
})

test('refreshing a shrunken last page restores remaining vaults and empty onboarding', async () => {
  let total = 21
  const pages: number[] = []
  const api = mockApi({
    'GET /projects/7/linked-vaults': (request) => {
      const page = Number(new URL(request.url).searchParams.get('page'))
      pages.push(page)
      const first = (page - 1) * 20
      return jsonResponse({
        items: Array.from(
          { length: Math.max(0, Math.min(20, total - first)) },
          (_, i) => ({
            vault: {
              id: `vault_${first + i}`,
              name: `Shared vault ${first + i}`,
              service_ids: [],
            },
            services: [],
          })
        ),
        total,
        page,
        page_size: 20,
      })
    },
    'GET /access-sessions': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <ProjectVaultAccess projectId={7} />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() => expect(ui.getByText('Shared vault 0')).toBeTruthy())
    fireEvent.click(ui.getByRole('button', { name: 'Next page' }))
    await waitFor(() => expect(ui.getByText('Shared vault 20')).toBeTruthy())
    total = 20
    await act(async () => {
      await client.invalidateQueries()
    })
    await waitFor(() => expect(ui.getByText('Shared vault 0')).toBeTruthy())
    expect(pages.slice(-1)).toEqual([1])
    expect(ui.queryByText('Shared vault 20')).toBeNull()
    total = 0
    await act(async () => {
      await client.invalidateQueries()
    })
    await waitFor(() =>
      expect(ui.getByRole('link', { name: 'Open vaults' })).toBeTruthy()
    )
  } finally {
    cleanup()
    client.clear()
    api.restore()
  }
})
