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
import {
  BreadcrumbProvider,
  useBreadcrumbs,
} from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import AccessSessionCreatePage from './AccessSessionCreatePage'
installBrowserWindow()
afterEach(async () => {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 20))
    cleanup()
  })
})
const date = '2026-01-01T00:00:00Z'
function BreadcrumbLinks() {
  const { breadcrumbs } = useBreadcrumbs()
  return (
    <nav aria-label="Breadcrumbs">
      {breadcrumbs
        .filter((item) => item.href)
        .map((item, index) => (
          <a key={index} href={item.href}>
            {item.label}
          </a>
        ))}
    </nav>
  )
}
const service = {
  id: 'svc_test',
  name: 'Example API',
  version: 2,
  project_id: 7,
  policy: {
    origin: 'https://api.example.com',
    methods: ['GET'],
    path_prefixes: ['/v1'],
    provider: 'github',
  },
  enabled: true,
  has_credential: true,
  max_ttl_seconds: 300,
  created_at: date,
  updated_at: date,
}
const bundle = {
  id: 'bundle_test',
  vault_id: 'vault_test',
  project_id: 7,
  name: 'Read APIs',
  service_ids: ['svc_test'],
  version: 3,
  ttl_seconds: 300,
  created_at: date,
  updated_at: date,
}
test.each([false, true])(
  'session approval preserves consent identity on retry (legacy project route: %s)',
  async (legacy) => {
    const bodies: unknown[] = []
    const handler = async (request: Request) => {
      const url = new URL(request.url)
      const method = request.method
      if (url.pathname.includes('/consumers/'))
        return jsonResponse({
          id: 'link_fixture',
          vault_id: 'vault_test',
          sandbox_id: legacy ? null : 42,
          project_id: legacy ? 7 : null,
          created_at: date,
        })
      if (method === 'POST') {
        bodies.push(await request.json())
        return jsonResponse({ detail: 'Policy changed. Review again.' }, 409)
      }
      if (url.pathname.endsWith('/capability'))
        return jsonResponse({
          configured: true,
          can_manage_requests: true,
          can_manage_services: !legacy,
        })
      if (url.pathname.endsWith('/bundles'))
        return jsonResponse({
          items: [bundle],
          total: 1,
          page: 1,
          page_size: 100,
        })
      if (url.pathname.endsWith('/access-vaults/vault_test'))
        return jsonResponse({
          vault: {
            id: 'vault_test',
            name: 'Work',
            project_id: 7,
            service_ids: ['svc_test'],
            version: 1,
          },
          services: [service],
        })
      if (url.pathname.endsWith('/v1/sandboxes'))
        return jsonResponse({
          sandboxes: [
            { id: 'sbx_ready', name: 'Ready sandbox', status: 'running' },
            { id: 'sbx_stopped', name: 'Stopped sandbox', status: 'stopped' },
          ],
          pagination: { count: 2 },
        })
      return jsonResponse({ detail: 'Unexpected request' }, 500)
    }
    const readBase = legacy
      ? '/projects/7/access-vaults/vault_test'
      : '/access-vaults/vault_test'
    const routeBase = legacy ? '/vault/projects/7/vaults' : '/vault/vaults'
    const contextQuery = legacy ? '' : '?sandbox=sbx_ready'
    const api = mockApi(
      Object.fromEntries(
        [
          'POST /access-vaults/vault_test/sessions',
          'GET /service-access/capability',
          `GET ${readBase}/bundles`,
          `GET ${readBase}`,
          'GET /v1/sandboxes',
          'GET /access-vaults/vault_test/consumers/sbx_ready',
        ].map((path) => [path, handler])
      )
    )
    try {
      const client = new QueryClient({
        defaultOptions: {
          queries: { retry: false },
          mutations: { retry: false },
        },
      })
      render(
        <QueryClientProvider client={client}>
          <MemoryRouter
            initialEntries={[
              `${routeBase}/vault_test/bundles/bundle_test/sessions/new${contextQuery}`,
            ]}
          >
            <BreadcrumbProvider>
              <BreadcrumbLinks />
              <Routes>
                <Route
                  path={`${legacy ? '/vault/projects/:projectId/vaults' : '/vault/vaults'}/:vaultId/bundles/:bundleId/sessions/new`}
                  element={<AccessSessionCreatePage />}
                />
              </Routes>
            </BreadcrumbProvider>
          </MemoryRouter>
        </QueryClientProvider>
      )
      const ui = within(document.body)
      await waitFor(() =>
        expect(ui.getByLabelText(/Ready sandbox/)).toBeTruthy()
      )
      await waitFor(() =>
        expect(
          within(ui.getByRole('navigation', { name: 'Breadcrumbs' }))
            .getByRole('link', { name: 'Work' })
            .getAttribute('href')
        ).toBe(`${routeBase}/vault_test${contextQuery}`)
      )
      expect(
        (ui.getByLabelText(/Stopped sandbox/) as HTMLInputElement).disabled
      ).toBe(true)
      fireEvent.click(ui.getByLabelText(/Ready sandbox/))
      fireEvent.change(ui.getByLabelText('Session lifetime (seconds)'), {
        target: { value: '120' },
      })
      await waitFor(() =>
        expect(
          ui
            .getByRole('button', { name: 'Approve session' })
            .getAttribute('aria-disabled')
        ).toBe('false')
      )
      fireEvent.click(ui.getByRole('button', { name: 'Approve session' }))
      await waitFor(() => expect(bodies.length).toBe(1))
      expect(bodies[0]).toMatchObject({
        bundle_id: 'bundle_test',
        bundle_version: 3,
        consumer: { kind: 'sandbox', sandbox_id: 'sbx_ready' },
        ttl_seconds: 120,
        services: [{ service_id: 'svc_test', version: 2 }],
      })
      await waitFor(() =>
        expect(ui.getByText('Could not issue session')).toBeTruthy()
      )
      expect(
        (ui.getByLabelText('Session lifetime (seconds)') as HTMLInputElement)
          .value
      ).toBe('120')
      fireEvent.click(ui.getByRole('button', { name: 'Approve session' }))
      await waitFor(() => expect(bodies.length).toBe(2))
      expect(bodies[1]).toEqual(bodies[0])
    } finally {
      api.restore()
    }
  }
)

test('a host that cannot host service access says so and stops offering the same approval', async () => {
  const bodies: unknown[] = []
  const handler = async (request: Request) => {
    const url = new URL(request.url)
    if (url.pathname.includes('/consumers/'))
      return jsonResponse({
        id: 'link_fixture',
        vault_id: 'vault_test',
        sandbox_id: 42,
        project_id: null,
        created_at: date,
      })
    if (request.method === 'POST') {
      bodies.push(await request.json())
      return jsonResponse(
        {
          title: 'Service access is unavailable for this sandbox',
          detail:
            'this control plane cannot share Unix sockets with the Docker daemon',
        },
        409
      )
    }
    if (url.pathname.endsWith('/capability'))
      return jsonResponse({
        configured: true,
        can_manage_requests: true,
        can_manage_services: true,
      })
    if (url.pathname.endsWith('/bundles'))
      return jsonResponse({
        items: [bundle],
        total: 1,
        page: 1,
        page_size: 100,
      })
    if (url.pathname.endsWith('/sandboxes'))
      return jsonResponse({
        sandboxes: [
          { id: 'sbx_ready', name: 'Ready sandbox', status: 'running' },
          { id: 'sbx_other', name: 'Other sandbox', status: 'running' },
        ],
        pagination: { page: 1, page_size: 20, count: 2 },
      })
    return jsonResponse({
      vault: { id: 'vault_test', name: 'Work', service_ids: ['svc_test'] },
      services: [service],
    })
  }
  const api = mockApi(
    Object.fromEntries(
      [
        'POST /access-vaults/vault_test/sessions',
        'GET /service-access/capability',
        'GET /access-vaults/vault_test/bundles',
        'GET /access-vaults/vault_test',
        'GET /v1/sandboxes',
        'GET /access-vaults/vault_test/consumers/sbx_ready',
        'GET /access-vaults/vault_test/consumers/sbx_other',
      ].map((path) => [path, handler])
    )
  )
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  try {
    render(
      <QueryClientProvider client={client}>
        <MemoryRouter
          initialEntries={[
            '/vault/vaults/vault_test/bundles/bundle_test/sessions/new?sandbox=sbx_ready',
          ]}
        >
          <BreadcrumbProvider>
            <Routes>
              <Route
                path="/vault/vaults/:vaultId/bundles/:bundleId/sessions/new"
                element={<AccessSessionCreatePage />}
              />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    const ui = within(document.body)
    await waitFor(() =>
      expect(
        ui
          .getByRole('button', { name: 'Approve session' })
          .getAttribute('aria-disabled')
      ).toBe('false')
    )
    fireEvent.click(ui.getByRole('button', { name: 'Approve session' }))
    await waitFor(() =>
      expect(
        ui.getByText("This sandbox can't host service access")
      ).toBeTruthy()
    )
    expect(ui.queryByText('Could not issue session')).toBeNull()
    expect(
      ui
        .getByRole('button', { name: 'Approve session' })
        .getAttribute('aria-disabled')
    ).toBe('true')
    // Choosing another sandbox is a new decision, so the refusal is cleared.
    fireEvent.click(ui.getByLabelText(/Other sandbox/))
    await waitFor(() =>
      expect(
        ui.queryByText("This sandbox can't host service access")
      ).toBeNull()
    )
    expect(bodies.length).toBe(1)
  } finally {
    api.restore()
  }
})
