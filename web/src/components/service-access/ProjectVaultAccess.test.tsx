// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { projectAccessVaultsOptions } from '@/api/client/@tanstack/react-query.gen'
import { ProjectVaultAccess } from './ProjectVaultAccess'

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
