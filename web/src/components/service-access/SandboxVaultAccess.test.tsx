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
  listSandboxServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { SandboxVaultAccess } from './SandboxVaultAccess'

const emptyPage = { items: [], total: 0, page: 1, page_size: 20 }

function client() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

function render(queryClient: QueryClient, projectId?: number) {
  return renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <SandboxVaultAccess projectId={projectId} sandboxId="sbx_test" />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function seed(queryClient: QueryClient) {
  queryClient.setQueryData(
    accessCapabilityOptions({ path: { project_id: 7 } }).queryKey,
    {
      configured: true,
      can_save_requests: true,
      can_manage_services: true,
      can_use_project_github: false,
      can_manage_requests: true,
      state: 'available',
      reason: 'Approve services for this sandbox.',
      setup_path: null,
    }
  )
  queryClient.setQueryData(
    listAccessServicesOptions({
      path: { project_id: 7 },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    emptyPage
  )
  queryClient.setQueryData(
    listSandboxServicesOptions({
      path: { project_id: 7, sandbox_id: 'sbx_test' },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    emptyPage
  )
  queryClient.setQueryData(
    listServiceGrantsOptions({
      path: { project_id: 7, sandbox_id: 'sbx_test' },
      query: { page: 1, page_size: 20 },
    }).queryKey,
    {
      items: [
        {
          id: 'grant_test',
          service_id: 'svc_test',
          service_name: 'Example API',
          sandbox_id: 'sbx_test',
          service_version: 1,
          version: 1,
          policy: {
            origin: 'https://api.example.com',
            path_prefixes: ['/v1'],
            methods: ['GET'],
          },
          status: 'approved',
          enforcement: 'active',
          reason: 'Read the example API.',
          connected: true,
          created_at: '2026-01-01T00:00:00Z',
          expires_at: '2026-01-01T00:15:00Z',
          revoked_at: null,
        },
      ],
      total: 1,
      page: 1,
      page_size: 20,
    }
  )
}

test("the Services tab lists the sandbox's time-limited access requests", () => {
  const c = client()
  seed(c)
  const html = render(c, 7)
  expect(html).toContain('Time-limited access requests')
  expect(html).toContain('Sandbox access requests')
  expect(html).toContain('Example API')
  expect(html).toContain('Revoke request')
  c.clear()
})

test('services are edited in Vault, not from the sandbox requests panel', () => {
  const c = client()
  seed(c)
  const html = render(c, 7)
  expect(html).toContain('Manage vaults')
  expect(html).not.toContain('Add service')
  expect(html).not.toContain('Edit service')
  c.clear()
})

test('a sandbox without a project explains the requirement once', () => {
  const c = client()
  const html = render(c)
  expect(html.split('A project is required').length - 1).toBe(1)
  expect(html).not.toContain('Sandbox access requests')
  c.clear()
})
