// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { listCustomDomainsForProjectQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DomainsSettings } from './DomainsSettings'

installBrowserWindow()
afterEach(cleanup)

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

const servedProject = {
  id: 7,
  slug: 'shop',
  hosting: 'temps',
  source_type: 'git',
} as unknown as ProjectResponse

const appDomain = {
  id: 4,
  domain: 'app.example.test',
  status: 'active',
  project_id: 7,
  created_at: 0,
  updated_at: 0,
  environment: { id: 3, slug: 'production' },
}

function mockProject(routes: Record<string, () => Response>) {
  return mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([{ id: 3, name: 'production', slug: 'production' }]),
    'GET /projects/7/environments/3/reachability': () =>
      jsonResponse({
        state: 'reachable',
        summary: '',
        addresses: [
          {
            address: 'app.example.test',
            audience: 'public',
            source: 'custom_domain',
            custom_domain_id: 4,
            way_in: 'direct:control-plane',
            state: 'reachable',
            reason: '',
            action: null,
            network: null,
            nothing_set_up: false,
            findings: [],
          },
        ],
      }),
    'GET /projects/7/environments/3/reachability/runs': () => jsonResponse([]),
    ...routes,
  })
}

function renderServed() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <DomainsSettings project={servedProject} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a failed domain list still shows every environment address, beside the error', async () => {
  mockProject({
    'GET /projects/7/custom-domains': () =>
      jsonResponse({ title: 'Unavailable', detail: 'Database is down' }, 503),
  })
  const view = renderServed()

  await view.findByRole('link', { name: 'app.example.test' })
  expect(view.getByRole('heading', { name: 'production' })).toBeTruthy()
  await view.findByRole('button', { name: 'Retry' })
})

test("deleting a domain refreshes this project's reachability with it", async () => {
  const api = mockProject({
    'GET /projects/7/custom-domains': () =>
      jsonResponse({ domains: [appDomain], total: 1 }),
    'DELETE /projects/7/custom-domains/4': () => jsonResponse({}),
  })
  const view = renderServed()
  const reachabilityReads = () =>
    api.calls.filter(
      (call) => call.path === '/projects/7/environments/3/reachability'
    ).length

  fireEvent.pointerDown(
    await view.findByRole('button', { name: 'Actions for app.example.test' }),
    { button: 0, ctrlKey: false }
  )
  fireEvent.click(await view.findByRole('menuitem', { name: 'Delete' }))
  const before = reachabilityReads()
  fireEvent.click(await view.findByRole('button', { name: /^Delete/ }))

  await waitFor(() =>
    expect(api.called('DELETE', '/projects/7/custom-domains/4')).toBe(true)
  )
  await waitFor(() => expect(reachabilityReads()).toBeGreaterThan(before))
})
