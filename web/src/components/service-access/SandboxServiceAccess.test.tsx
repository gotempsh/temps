// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render as mount,
  within,
} from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'

installBrowserWindow()
afterEach(cleanup)
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, useLocation } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  accessCapabilityOptions,
  listAccessServicesOptions,
  listServiceGrantsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { SandboxServiceAccess } from './SandboxServiceAccess'
import type { AccessServiceResponse } from '@/api/client'

function client() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}
function render(queryClient: QueryClient, projectId?: number, entry = '/') {
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter initialEntries={[entry]}>
        <SandboxServiceAccess projectId={projectId} sandboxId="sbx_test" />
      </MemoryRouter>
    </QueryClientProvider>
  )
}
const capabilityKey = accessCapabilityOptions({
  path: { project_id: 7 },
}).queryKey
const serviceKey = listAccessServicesOptions({
  path: { project_id: 7 },
  query: { page: 1, page_size: 20 },
}).queryKey
const grantKey = listServiceGrantsOptions({
  path: { project_id: 7, sandbox_id: 'sbx_test' },
  query: { page: 1, page_size: 20 },
}).queryKey

test('an unlinked sandbox explains how to reach the feature', () => {
  const c = client()
  const html = render(c)
  expect(html).toContain('A project is required')
  expect(html).toContain('Open projects')
  expect(html).toContain('Service access')
  c.clear()
})
test('unavailable integration remains discoverable and allows saving intents', () => {
  const c = client()
  c.setQueryData(capabilityKey, {
    configured: false,
    can_save_requests: true,
    can_manage_services: true,
    can_use_project_github: false,
    can_manage_requests: true,
    state: 'integration_unavailable',
    reason: 'The Tokenless integration is not installed.',
    setup_path: null,
  })
  c.setQueryData(serviceKey, { items: [], total: 0, page: 1, page_size: 20 })
  c.setQueryData(grantKey, { items: [], total: 0, page: 1, page_size: 20 })
  const html = render(c, 7)
  expect(html).toContain('The Tokenless integration is not installed.')
  expect(html).toContain('Add service')
  expect(html).toContain('No access requests')
  expect(html).not.toContain('Connected')
  c.clear()
})
test('read errors are shown as errors instead of empty collections', () => {
  const c = client()
  for (const queryKey of [capabilityKey, serviceKey, grantKey]) {
    c.getQueryCache()
      .build(c, { queryKey })
      .setState({
        status: 'error',
        error: new Error('network failed'),
        fetchStatus: 'idle',
      })
  }
  const html = render(c, 7)
  expect(html).toContain('Could not load service access')
  expect(html).toContain('Retry')
  expect(html).not.toContain('No services saved')
  expect(html).not.toContain('No access requests')
  c.clear()
})
test('a stale-policy request displays its reason and absolute expiry', () => {
  const c = client()
  c.setQueryData(capabilityKey, {
    configured: false,
    can_save_requests: true,
    can_manage_services: true,
    can_use_project_github: false,
    can_manage_requests: true,
    state: 'integration_unavailable',
    reason: 'Integration unavailable.',
    setup_path: null,
  })
  c.setQueryData(serviceKey, { items: [], total: 0, page: 1, page_size: 20 })
  c.setQueryData(grantKey, {
    items: [
      {
        id: 'grant_test',
        service_id: 'svc_test',
        service_name: 'API',
        sandbox_id: 'sbx_test',
        service_version: 1,
        version: 1,
        policy: {
          origin: 'https://api.example.com',
          path_prefixes: ['/v1'],
          methods: ['GET'],
        },
        status: 'policy_changed',
        enforcement: 'not_requested',
        reason: 'Review the changed policy.',
        connected: false,
        created_at: '2026-01-01T00:00:00Z',
        expires_at: '2026-01-01T00:15:00Z',
        revoked_at: null,
      },
    ],
    total: 1,
    page: 1,
    page_size: 20,
  })
  const html = render(c, 7)
  expect(html).toContain('Policy changed')
  expect(html).toContain('Review the changed policy.')
  expect(html).toContain('Expires')
  expect(html).toContain('Revoke request')
  c.clear()
})

test.each([0, 1])(
  'an empty page preserves pagination when the collection has %s rows',
  (total) => {
    const c = client()
    c.setQueryData(
      listAccessServicesOptions({
        path: { project_id: 7 },
        query: { page: 2, page_size: 20 },
      }).queryKey,
      { items: [], total, page: 2, page_size: 20 }
    )
    c.setQueryData(
      listServiceGrantsOptions({
        path: { project_id: 7, sandbox_id: 'sbx_test' },
        query: { page: 2, page_size: 20 },
      }).queryKey,
      { items: [], total, page: 2, page_size: 20 }
    )
    const html = render(c, 7, '/?accessServicesPage=2&accessRequestsPage=2')
    expect(html).not.toContain('No services saved')
    expect(html).not.toContain('No access requests')
    expect(html).toContain('Service pages')
    expect(html).toContain('Access request pages')
    expect(html).toContain('Previous')
    c.clear()
  }
)

const testService: AccessServiceResponse = {
  id: 'svc_test',
  project_id: 7,
  name: 'Example API',
  version: 1,
  enabled: true,
  has_credential: true,
  max_ttl_seconds: 1800,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
  policy: {
    origin: 'https://api.example.com',
    path_prefixes: ['/v1'],
    methods: ['GET'],
  },
}
function seed(
  c: QueryClient,
  manageServices: boolean,
  manageRequests: boolean
) {
  c.setQueryData(capabilityKey, {
    configured: false,
    can_save_requests: true,
    can_manage_services: manageServices,
    can_use_project_github: false,
    can_manage_requests: manageRequests,
    state: 'integration_unavailable',
    reason: 'Integration unavailable.',
    setup_path: null,
  })
  c.setQueryData(serviceKey, {
    items: [testService],
    total: 1,
    page: 1,
    page_size: 20,
  })
  c.setQueryData(grantKey, { items: [], total: 0, page: 1, page_size: 20 })
}

test('a configured service opens explicit approval for the selected sandbox', async () => {
  const c = client()
  seed(c, true, true)
  c.setQueryData(capabilityKey, {
    configured: true,
    can_save_requests: true,
    can_manage_services: true,
    can_use_project_github: false,
    can_manage_requests: true,
    state: 'available',
    reason: 'Approve a service for the assigned worker.',
    setup_path: null,
  })
  mount(
    <QueryClientProvider client={c}>
      <MemoryRouter>
        <SandboxServiceAccess projectId={7} sandboxId="sbx_test" />
      </MemoryRouter>
    </QueryClientProvider>
  )
  await act(async () =>
    fireEvent.click(
      within(document.body).getByRole('button', { name: 'Grant access' })
    )
  )
  const dialog = within(document.body).getByRole('dialog')
  expect(document.body.textContent).toContain(
    'Use an approved service from Terminal'
  )
  expect(document.body.textContent).toContain(
    '/run/temps-service-access/ca.pem'
  )
  expect(dialog.textContent).toContain('Sandbox: sbx_test')
  expect(dialog.textContent).toContain('https://api.example.com')
  expect(
    within(dialog).getByRole('button', { name: 'Approve access' })
  ).toBeTruthy()
  expect(
    within(dialog).queryByRole('button', { name: 'Save access request' })
  ).toBeNull()
  c.clear()
})

test.each([
  ['pending', 'Connecting'],
  ['active', 'Connected'],
  ['stopping', 'Revoking'],
  ['stopped', 'Disconnected'],
] as const)(
  'worker enforcement %s determines the displayed status',
  (enforcement, label) => {
    const c = client()
    seed(c, true, true)
    c.setQueryData(grantKey, {
      items: [
        {
          id: 'grant_test',
          service_id: testService.id,
          service_name: testService.name,
          sandbox_id: 'sbx_test',
          service_version: 1,
          version: 1,
          policy: testService.policy,
          status: 'approved',
          enforcement,
          connected: enforcement === 'active',
          reason: 'Worker receipt status.',
          created_at: '2026-01-01T00:00:00Z',
          expires_at: '2026-01-01T00:30:00Z',
          revoked_at: null,
        },
      ],
      total: 1,
      page: 1,
      page_size: 20,
    })
    expect(render(c, 7)).toContain(label)
    c.clear()
  }
)

test.each([
  [false, false],
  [true, false],
  [true, true],
])(
  'write actions follow effective permissions (%s, %s)',
  async (services, requests) => {
    const c = client()
    seed(c, services, requests)
    mount(
      <QueryClientProvider client={c}>
        <MemoryRouter>
          <SandboxServiceAccess projectId={7} sandboxId="sbx_test" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(
      (
        within(document.body).getByRole('button', {
          name: 'Add service',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(!services)
    expect(
      (
        within(document.body).getByRole('button', {
          name: 'Request access',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(!requests)
    if (!requests)
      expect(document.body.textContent).toContain(
        'Access requests are read-only'
      )
    expect(
      within(document.body).queryByRole('button', { name: 'Edit' })
    ).toBeNull()
    await act(async () => {
      fireEvent.pointerDown(
        within(document.body).getByRole('button', {
          name: 'Actions for Example API',
        }),
        { button: 0, ctrlKey: false }
      )
    })
    expect(
      within(document.body)
        .getByRole('menuitem', { name: 'Edit service' })
        .getAttribute('aria-disabled') === 'true'
    ).toBe(!services)
    c.clear()
  }
)

test('the secondary service action opens the routed editor from its menu', async () => {
  const c = client()
  seed(c, true, true)
  function Location() {
    const location = useLocation()
    return (
      <output aria-label="Location">
        {location.pathname}
        {location.search}
      </output>
    )
  }
  mount(
    <QueryClientProvider client={c}>
      <MemoryRouter>
        <SandboxServiceAccess projectId={7} sandboxId="sbx_test" />
        <Location />
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(
    within(document.body).queryByRole('menuitem', { name: 'Edit service' })
  ).toBeNull()
  await act(async () => {
    fireEvent.pointerDown(
      within(document.body).getByRole('button', {
        name: 'Actions for Example API',
      }),
      { button: 0, ctrlKey: false }
    )
  })
  await act(async () => {
    fireEvent.click(
      within(document.body).getByRole('menuitem', { name: 'Edit service' })
    )
  })
  expect(within(document.body).getByLabelText('Location').textContent).toBe(
    '/access-services/svc_test/edit?sandbox=sbx_test'
  )
  c.clear()
})

test('changing to a cached sandbox closes the old request dialog', async () => {
  const c = client()
  seed(c, true, true)
  c.setQueryData(
    listServiceGrantsOptions({
      path: { project_id: 7, sandbox_id: 'sbx_other' },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    { items: [], total: 0, page: 1, page_size: 20 }
  )
  const view = (sandboxId: string) => (
    <QueryClientProvider client={c}>
      <MemoryRouter>
        <SandboxServiceAccess projectId={7} sandboxId={sandboxId} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const rendered = mount(view('sbx_test'))
  await act(async () =>
    fireEvent.click(
      within(document.body).getByRole('button', { name: 'Request access' })
    )
  )
  expect(within(document.body).getByRole('dialog')).toBeTruthy()
  await act(async () => rendered.rerender(view('sbx_other')))
  expect(within(document.body).queryByRole('dialog')).toBeNull()
  expect(
    within(document.body).queryByRole('button', { name: 'Save access request' })
  ).toBeNull()
  c.clear()
})
