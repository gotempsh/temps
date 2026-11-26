// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { cleanup, render } from '@testing-library/react'
import type { ProjectResponse, ProjectSecretResponse } from '@/api/client'
import { getProjectSecretQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SecretPage } from './SecretPage'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = { id: 3, slug: 'shop' } as ProjectResponse

const SECRET: ProjectSecretResponse = {
  id: 5,
  project_id: 3,
  key: 'STRIPE_KEY',
  include_in_preview: false,
  compose_services: [],
  environments: [],
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

test('a secret deleted elsewhere shows as not found, not from the stale cache', async () => {
  const api = mockApi({
    'GET /projects/3/secrets/5': () =>
      jsonResponse(
        { status: 404, title: 'Not Found', detail: 'Secret 5 not found' },
        404
      ),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  // The page was open before: its last answer is still cached.
  client.setQueryData(
    getProjectSecretQueryKey({ path: { project_id: 3, secret_id: 5 } }),
    SECRET
  )
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/projects/shop/settings/secrets/5']}>
        <Routes>
          <Route
            path="/projects/shop/settings/secrets/:secretId"
            element={<SecretPage project={PROJECT} />}
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )

  expect(await view.findByText('Secret not found')).toBeTruthy()
  expect(view.queryByText('STRIPE_KEY')).toBeNull()
  api.restore()
})
