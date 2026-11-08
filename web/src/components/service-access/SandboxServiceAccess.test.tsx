// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  accessCapabilityOptions,
  listAccessServicesOptions,
  listServiceGrantsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { SandboxServiceAccess } from './SandboxServiceAccess'

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
