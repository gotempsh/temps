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
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import type {
  ApproveSandboxService,
  RevokeSandboxService,
  SandboxServiceResponse,
} from '@/api/client'
import { SandboxServices } from './SandboxServices'

installBrowserWindow()
afterEach(cleanup)

test('sandbox lifetime approval and revocation use exact versions without claiming worker activation', async () => {
  let row: SandboxServiceResponse = {
    id: 'access_test',
    service_id: 'svc_test',
    service_name: 'Example API',
    service_version: 4,
    policy: {
      origin: 'https://api.example.com',
      path_prefixes: ['/v1'],
      methods: ['GET'],
    },
    version: 1,
    status: 'pending_approval',
    enforcement: 'not_requested',
    connected: false,
    reason: 'Needs approval',
    expires_at: null,
    created_at: '2026-01-01T00:00:00Z',
  }
  let approval: ApproveSandboxService | undefined
  let revocation: RevokeSandboxService | undefined
  const api = mockApi({
    'GET /projects/7/service-access/capability': () =>
      jsonResponse({
        configured: true,
        reason: 'Supported worker required',
        setup_path: '/settings',
      }),
    'GET /projects/7/access-services': () =>
      jsonResponse({ items: [], total: 0, page: 1, page_size: 20 }),
    'GET /projects/7/sandboxes/sbx_test/services': () =>
      jsonResponse({ items: [row], total: 1, page: 1, page_size: 20 }),
    'POST /projects/7/sandboxes/sbx_test/services/access_test/approve': async (
      request
    ) => {
      approval = (await request.json()) as ApproveSandboxService
      row = {
        ...row,
        version: 2,
        status: 'approved',
        reason: 'Waiting for worker',
      }
      return jsonResponse(row, 202)
    },
    'POST /projects/7/sandboxes/sbx_test/services/access_test/revoke': async (
      request
    ) => {
      revocation = (await request.json()) as RevokeSandboxService
      row = { ...row, version: 3, status: 'revoked', reason: 'Access revoked' }
      return jsonResponse(row, 202)
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    const view = render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <SandboxServices projectId={7} sandboxId="sbx_test" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    await waitFor(() =>
      expect(within(view.container).getByText('Awaiting approval')).toBeTruthy()
    )
    const usage = within(view.container)
      .getByText('Use an approved service from Terminal')
      .closest('details')
    expect(usage).toBeTruthy()
    expect(usage?.hidden).toBe(false)
    fireEvent.click(
      within(view.container).getByText('Use an approved service from Terminal')
    )
    expect(usage?.textContent).toContain('Wait for Active')
    expect(usage?.textContent).toContain('curl --proxy http://127.0.0.1:3128')
    expect(usage?.textContent).toContain(
      '--cacert /run/temps-service-access/ca.pem'
    )
    expect(usage?.textContent).toContain('https://api.example.com/v1')
    expect(usage?.textContent).toContain(
      'http.sslCAInfo=/run/temps-service-access/ca.pem'
    )
    await act(async () => {
      fireEvent.click(
        within(view.container).getByRole('button', { name: 'Approve access' })
      )
    })
    await waitFor(() =>
      expect(within(view.container).getByText('Connecting')).toBeTruthy()
    )
    expect(approval).toEqual({
      expected_version: 1,
      service_version: 4,
      expires_at: null,
    })
    expect(within(view.container).queryByText('Active')).toBeNull()
    await act(async () => {
      fireEvent.click(
        within(view.container).getByRole('button', { name: 'Revoke' })
      )
    })
    const dialog = within(document.body).getByRole('alertdialog')
    expect(dialog.textContent).toContain('Example API')
    expect(dialog.textContent).toContain('sbx_test')
    expect(revocation).toBeUndefined()
    await act(async () => {
      fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    })
    expect(revocation).toBeUndefined()
    await act(async () => {
      fireEvent.click(
        within(view.container).getByRole('button', { name: 'Revoke' })
      )
    })
    await act(async () => {
      fireEvent.click(
        within(within(document.body).getByRole('alertdialog')).getByRole(
          'button',
          { name: 'Revoke' }
        )
      )
    })
    await waitFor(() =>
      expect(within(view.container).getByText('Revoked')).toBeTruthy()
    )
    expect(revocation).toEqual({ expected_version: 2 })
    view.unmount()
  } finally {
    client.clear()
    api.restore()
  }
})

test('unavailable transport explains setup instead of showing an approved service as connecting', async () => {
  const api = mockApi({
    'GET /projects/7/service-access/capability': () =>
      jsonResponse({
        configured: false,
        reason:
          'Service access requires a supported Linux Docker worker. MicroVM is unavailable.',
        setup_path: '/settings/workers',
      }),
    'GET /projects/7/access-services': () =>
      jsonResponse({ items: [], total: 0 }),
    'GET /projects/7/sandboxes/sbx_test/services': () =>
      jsonResponse({
        items: [
          {
            id: 'access_example',
            service_name: 'Example API',
            status: 'approved',
            connected: false,
            policy: {
              origin: 'https://api.example.com',
              methods: ['GET'],
              path_prefixes: ['/v1'],
            },
            reason: 'Waiting for worker',
            expires_at: null,
          },
        ],
        total: 1,
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    const view = render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <SandboxServices projectId={7} sandboxId="sbx_test" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    await waitFor(() =>
      expect(within(view.container).getByText('Unavailable')).toBeTruthy()
    )
    expect(
      within(view.container).getByText('Sandbox connections unavailable')
    ).toBeTruthy()
    expect(
      within(view.container)
        .getByRole('link', { name: 'Set up service access' })
        .getAttribute('href')
    ).toBe('/settings/workers')
    expect(within(view.container).queryByText('Connecting')).toBeNull()
    expect(
      within(view.container).getAllByText(/MicroVM is unavailable/).length
    ).toBeGreaterThan(0)
    expect(
      within(view.container).getByRole('button', { name: 'Request access' })
    ).toBeTruthy()
    expect(
      within(view.container).getByRole('button', { name: 'Revoke' })
    ).toBeTruthy()
    view.unmount()
  } finally {
    client.clear()
    api.restore()
  }
})

test('a failed capability check offers retry while preserving the services surface', async () => {
  let attempts = 0
  const api = mockApi({
    'GET /projects/7/service-access/capability': () => {
      attempts += 1
      return attempts === 1
        ? jsonResponse({ detail: 'Worker support check failed' }, 503)
        : jsonResponse({
            configured: true,
            reason:
              'Linux Docker worker required; MicroVM and Docker Desktop are unsupported.',
          })
    },
    'GET /projects/7/access-services': () =>
      jsonResponse({ items: [], total: 0 }),
    'GET /projects/7/sandboxes/sbx_test/services': () =>
      jsonResponse({ items: [], total: 0 }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  try {
    const view = render(
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <SandboxServices projectId={7} sandboxId="sbx_test" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    await waitFor(() =>
      expect(
        within(view.container).getByText(
          'Could not check service access support'
        )
      ).toBeTruthy()
    )
    expect(
      within(view.container).getByRole('button', { name: 'Request access' })
    ).toBeTruthy()
    await act(async () => {
      fireEvent.click(
        within(view.container).getByRole('button', {
          name: 'Retry capability check',
        })
      )
    })
    await waitFor(() =>
      expect(
        within(view.container).getByText(
          /MicroVM and Docker Desktop are unsupported/
        )
      ).toBeTruthy()
    )
    expect(attempts).toBe(2)
    view.unmount()
  } finally {
    client.clear()
    api.restore()
  }
})
