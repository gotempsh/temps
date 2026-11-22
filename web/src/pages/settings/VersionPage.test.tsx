// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { QueryKey } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { getUpdateCapabilityOptions } from '@/api/client/@tanstack/react-query.gen'
import { VersionPage } from './VersionPage'

const refused = {
  title: 'Forbidden',
  status: 403,
  detail: 'Requires settings:read permission',
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

function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <BreadcrumbProvider>
          <VersionPage />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const capabilityKey = getUpdateCapabilityOptions().queryKey

test('a refused update capability read is not shown as an unknown version', () => {
  const client = createClient()
  fail(client, capabilityKey, refused)
  const html = render(client)
  expect(html).toContain('Version and update status: access denied')
  expect(html).toContain('Requires settings:read permission')
  expect(html).not.toContain('unknown')
  expect(html).not.toContain('updates restart temps automatically')
  client.clear()
})
