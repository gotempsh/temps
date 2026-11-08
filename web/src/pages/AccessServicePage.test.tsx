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
import type { AccessServiceResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import AccessServicePage from './AccessServicePage'

installBrowserWindow()
afterEach(cleanup)

const service: AccessServiceResponse = {
  id: 'svc_test',
  project_id: 7,
  name: 'API',
  policy: {
    origin: 'https://api.example.com',
    path_prefixes: ['/v1'],
    methods: ['GET'],
  },
  enabled: true,
  has_credential: true,
  max_ttl_seconds: 120,
  version: 4,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

function mount(entry: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route
            path="/projects/:projectId/access-services/new"
            element={<AccessServicePage />}
          />
          <Route
            path="/projects/:projectId/access-services/:serviceId/edit"
            element={<AccessServicePage />}
          />
          <Route
            path="/sandboxes/:sandboxId"
            element={<div>Returned to sandbox</div>}
          />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return client
}

test('direct editor URL loads masked service and preserves edits and version across refetch', async () => {
  let submitted:
    | { name?: string; expected_version?: number; credential?: string }
    | undefined
  let reads = 0
  const api = mockApi({
    'GET /projects/7/access-services/svc_test': () =>
      jsonResponse({ ...service, version: ++reads === 1 ? 4 : 5 }),
    'PUT /projects/7/access-services/svc_test': async (request) => {
      submitted = await request.json()
      return jsonResponse({ ...service, version: 5 })
    },
  })
  try {
    const client = mount(
      '/projects/7/access-services/svc_test/edit?sandbox=sbx_test'
    )
    await waitFor(() =>
      expect(within(document.body).getByLabelText('Name')).toBeTruthy()
    )
    expect(within(document.body).queryByRole('dialog')).toBeNull()
    await act(async () =>
      fireEvent.change(within(document.body).getByLabelText('Name'), {
        target: { value: 'Local edit' },
      })
    )
    await act(async () => {
      await client.invalidateQueries({
        queryKey: [{ _id: 'getAccessService' }],
      })
    })
    expect(reads).toBe(2)
    expect(
      (within(document.body).getByLabelText('Name') as HTMLInputElement).value
    ).toBe('Local edit')
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Save service' })
      )
    )
    await waitFor(() =>
      expect(document.body.textContent).toContain('Returned to sandbox')
    )
    expect(submitted?.name).toBe('Local edit')
    expect(submitted?.expected_version).toBe(4)
    expect(submitted?.credential).toBeUndefined()
    client.clear()
  } finally {
    api.restore()
  }
})

test('new service has a direct routed form without reading an existing service', () => {
  const client = mount('/projects/7/access-services/new?sandbox=sbx_test')
  expect(
    within(document.body).getByRole('heading', { name: 'Add service' })
  ).toBeTruthy()
  expect(within(document.body).queryByRole('dialog')).toBeNull()
  expect(
    (within(document.body).getByLabelText('Name') as HTMLInputElement).value
  ).toBe('')
  client.clear()
})

test('failed deep-link read shows error and retry instead of a blank creation form', async () => {
  const api = mockApi({
    'GET /projects/7/access-services/svc_test': () =>
      jsonResponse({ detail: 'Project access denied' }, 403),
  })
  try {
    const client = mount('/projects/7/access-services/svc_test/edit')
    await waitFor(() =>
      expect(document.body.textContent).toContain('Project access denied')
    )
    expect(
      within(document.body).getByRole('button', { name: 'Retry' })
    ).toBeTruthy()
    expect(within(document.body).queryByLabelText('Name')).toBeNull()
    client.clear()
  } finally {
    api.restore()
  }
})

test('invalid project URL is explained before any editor is displayed', () => {
  const client = mount('/projects/invalid/access-services/new')
  expect(document.body.textContent).toContain(
    'project ID in this link is invalid'
  )
  expect(within(document.body).queryByLabelText('Name')).toBeNull()
  client.clear()
})
