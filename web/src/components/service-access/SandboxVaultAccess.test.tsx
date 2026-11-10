// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  listAccessSessionsGeneralOptions,
  sandboxAccessVaultsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { SandboxVaultAccess } from './SandboxVaultAccess'

function markup(linked: boolean) {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false } },
  })
  client.setQueryData(
    sandboxAccessVaultsOptions({
      path: { sandbox_id: 'sbx_fixture' },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    {
      items: linked
        ? [
            {
              vault: {
                id: 'vault_fixture',
                version: 1,
                created_at: '2026-01-01T00:00:00Z',
                updated_at: '2026-01-01T00:00:00Z',
                name: 'Shared APIs',
                service_ids: [],
              },
              services: [],
            },
          ]
        : [],
      total: linked ? 1 : 0,
      page: 1,
      page_size: 20,
    }
  )
  client.setQueryData(
    listAccessSessionsGeneralOptions({
      query: {
        sandbox_id: 'sbx_fixture',
        vault_id: undefined,
        page: 1,
        page_size: 20,
      },
    }).queryKey,
    { items: [], total: 0, page: 1, page_size: 20 }
  )
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SandboxVaultAccess sandboxId="sbx_fixture" projectId={null} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}
test('projectless sandboxes can grant access from linked vaults', () => {
  const html = markup(true)
  expect(html).toContain('Grant access')
  expect(html).toContain(
    '/vault/vaults/vault_fixture?tab=bundles&amp;sandbox=sbx_fixture'
  )
  expect(html).not.toContain('A project is required')
})
test('unlinked sandboxes keep the grant-access onboarding visible', () => {
  const html = markup(false)
  expect(html).toContain('Grant access')
  expect(html).toContain('No vault is linked')
  expect(html).toContain('AI chat')
  expect(html).toContain('href="/vault"')
})
