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
import {
  accessCapabilityOptions,
  getAccessServiceOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  BreadcrumbProvider,
  useBreadcrumbs,
} from '@/contexts/BreadcrumbContext'
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

function BreadcrumbProbe() {
  const { breadcrumbs } = useBreadcrumbs()
  return (
    <output aria-label="Shared breadcrumbs">
      {JSON.stringify(breadcrumbs)}
    </output>
  )
}

function mount(
  entry: string,
  canManage = true,
  cachedService?: AccessServiceResponse
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  if (cachedService) {
    client.setQueryData(
      getAccessServiceOptions({
        path: { project_id: 7, service_id: cachedService.id },
      }).queryKey,
      cachedService
    )
  }
  client.setQueryData(
    accessCapabilityOptions({ path: { project_id: 7 } }).queryKey,
    {
      configured: false,
      can_save_requests: true,
      can_manage_services: canManage,
      can_manage_requests: canManage,
      state: 'integration_unavailable',
      reason: 'Integration unavailable.',
      setup_path: null,
    }
  )
  client.setQueryDefaults(
    accessCapabilityOptions({ path: { project_id: 7 } }).queryKey,
    { staleTime: Infinity }
  )
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <BreadcrumbProvider>
          <BreadcrumbProbe />
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
        </BreadcrumbProvider>
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
    within(document.body).getByLabelText('Shared breadcrumbs').textContent
  ).toContain('Add service')
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

test('a read-only caller cannot open the creation editor by URL', () => {
  const client = mount('/projects/7/access-services/new', false)
  expect(document.body.textContent).toContain('Service policies are read-only')
  expect(within(document.body).queryByLabelText('Name')).toBeNull()
  expect(
    within(document.body).queryByRole('button', { name: 'Save service' })
  ).toBeNull()
  client.clear()
})

test('a failed capability refresh preserves edits through retry', async () => {
  let unavailable = true
  const api = mockApi({
    'GET /projects/7/access-services/svc_test': () => jsonResponse(service),
    'GET /projects/7/service-access/capability': () =>
      unavailable
        ? jsonResponse({ detail: 'Permissions temporarily unavailable' }, 503)
        : jsonResponse({
            configured: false,
            can_save_requests: true,
            can_manage_services: true,
            can_manage_requests: true,
            state: 'integration_unavailable',
            reason: 'Integration unavailable.',
            setup_path: null,
          }),
  })
  try {
    const client = mount('/projects/7/access-services/svc_test/edit')
    await waitFor(() =>
      expect(within(document.body).getByLabelText('Name')).toBeTruthy()
    )
    const field = within(document.body).getByLabelText(
      'Name'
    ) as HTMLInputElement
    await act(async () =>
      fireEvent.change(field, { target: { value: 'Unsaved edit' } })
    )
    await act(async () => {
      await client.invalidateQueries({
        queryKey: [{ _id: 'accessCapability' }],
      })
    })
    await waitFor(() =>
      expect(document.body.textContent).toContain('Your edits are preserved')
    )
    expect(within(document.body).getByLabelText('Name')).toBe(field)
    expect(field.value).toBe('Unsaved edit')
    expect(field.closest('fieldset[disabled]')).not.toBeNull()
    unavailable = false
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Retry permissions' })
      )
    )
    await waitFor(() =>
      expect(
        within(document.body).queryByRole('button', {
          name: 'Retry permissions',
        })
      ).toBeNull()
    )
    expect(within(document.body).getByLabelText('Name')).toBe(field)
    expect(field.value).toBe('Unsaved edit')
    expect(field.closest('fieldset[disabled]')).toBeNull()
    client.clear()
  } finally {
    api.restore()
  }
})

test('service editor routes populate and clear the shared breadcrumb trail', async () => {
  const api = mockApi({
    'GET /projects/7/access-services/svc_test': () => jsonResponse(service),
  })
  try {
    const client = mount(
      '/projects/7/access-services/svc_test/edit?sandbox=sbx_test'
    )
    await waitFor(() =>
      expect(
        within(document.body).getByLabelText('Shared breadcrumbs').textContent
      ).toContain('API')
    )
    expect(
      JSON.parse(
        within(document.body).getByLabelText('Shared breadcrumbs')
          .textContent ?? '[]'
      )
    ).toEqual([
      { label: 'Projects', href: '/projects' },
      { label: 'Project 7', href: '/projects/7' },
      { label: 'API' },
    ])
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Cancel' })
      )
    )
    expect(
      within(document.body).getByLabelText('Shared breadcrumbs').textContent
    ).toBe('[]')
    client.clear()
  } finally {
    api.restore()
  }
})

test('reopening a cached service waits for a fresh version before editing', async () => {
  let finishRead: ((response: Response) => void) | undefined
  let submittedVersion: number | undefined
  const api = mockApi({
    'GET /projects/7/access-services/svc_test': () =>
      new Promise<Response>((resolve) => {
        finishRead = resolve
      }),
    'PUT /projects/7/access-services/svc_test': async (request) => {
      const body = await request.json()
      submittedVersion = body.expected_version
      return jsonResponse({ ...service, version: 6 })
    },
  })
  try {
    const client = mount(
      '/projects/7/access-services/svc_test/edit?sandbox=sbx_test',
      true,
      service
    )
    expect(within(document.body).queryByLabelText('Name')).toBeNull()
    await waitFor(() => expect(finishRead).toBeDefined())
    await act(async () =>
      finishRead?.(
        jsonResponse({ ...service, name: 'Current policy', version: 5 })
      )
    )
    await waitFor(() =>
      expect(
        (within(document.body).getByLabelText('Name') as HTMLInputElement).value
      ).toBe('Current policy')
    )
    await act(async () =>
      fireEvent.change(within(document.body).getByLabelText('Name'), {
        target: { value: 'Edited current policy' },
      })
    )
    await act(async () =>
      fireEvent.click(
        within(document.body).getByRole('button', { name: 'Save service' })
      )
    )
    await waitFor(() => expect(submittedVersion).toBe(5))
    client.clear()
  } finally {
    api.restore()
  }
})

test('an initial refresh failure cannot open an obsolete cached policy', async () => {
  const api = mockApi({
    'GET /projects/7/access-services/svc_test': () =>
      jsonResponse({ detail: 'Read unavailable' }, 503),
  })
  try {
    const client = mount(
      '/projects/7/access-services/svc_test/edit',
      true,
      service
    )
    await waitFor(() =>
      expect(document.body.textContent).toContain('Read unavailable')
    )
    expect(within(document.body).queryByLabelText('Name')).toBeNull()
    client.clear()
  } finally {
    api.restore()
  }
})
