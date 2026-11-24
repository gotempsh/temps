// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { listCustomDomainsForProjectQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { DomainsSettings } from './DomainsSettings'

test('only domains displaced by a platform address show the conflict message', () => {
  const project = {
    id: 1,
    slug: 'shop',
    hosting: 'external',
  } as ProjectResponse
  const client = new QueryClient()
  const message =
    'A deployment owns this hostname. Rename or delete this domain.'
  client.setQueryData(
    listCustomDomainsForProjectQueryKey({ path: { project_id: project.id } }),
    {
      domains: [
        {
          id: 1,
          domain: 'api.apps.example.com',
          status: 'hostname_conflict',
          message,
        },
        {
          id: 2,
          domain: 'shop.apps.example.com',
          status: 'active',
          message: 'Certificate issued.',
        },
      ],
      total: 2,
    }
  )
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <DomainsSettings project={project} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(html).toContain(message)
  expect(html).toContain('shop.apps.example.com')
  expect(html).not.toContain('Certificate issued.')
})
