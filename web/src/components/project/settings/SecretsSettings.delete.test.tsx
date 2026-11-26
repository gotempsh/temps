// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import type { ProjectResponse, ProjectSecretResponse } from '@/api/client'
import { getProjectSecretQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SecretsSettings } from './SecretsSettings'

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

test('deleting a secret drops its cached read, so its page cannot show it', async () => {
  let deleted = false
  const api = mockApi({
    'GET /projects/3/secrets': () =>
      jsonResponse({
        items: deleted ? [] : [SECRET],
        total: deleted ? 0 : 1,
        page: 1,
        page_size: 20,
      }),
    'GET /projects/3/environments': () => jsonResponse([]),
    'DELETE /projects/3/secrets/5': () => {
      deleted = true
      return new Response(null, { status: 204 })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const secretKey = getProjectSecretQueryKey({
    path: { project_id: 3, secret_id: 5 },
  })
  // The secret's page was visited before: its answer is cached.
  client.setQueryData(secretKey, SECRET)
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <SecretsSettings project={PROJECT} />
      </MemoryRouter>
    </QueryClientProvider>
  )

  await act(async () => {
    fireEvent.click(await view.findByLabelText('Delete secret STRIPE_KEY'))
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Delete' }))
  })

  await waitFor(() =>
    expect(api.called('DELETE', '/projects/3/secrets/5')).toBe(true)
  )
  await waitFor(() => expect(client.getQueryState(secretKey)).toBeUndefined())
  api.restore()
})
