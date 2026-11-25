// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EnvironmentReachabilityList } from './EnvironmentReachabilityList'

installBrowserWindow()
afterEach(cleanup)

const address = (host: string, state: string, wayIn: string | null) => ({
  address: host,
  audience: 'public',
  source: 'custom_domain',
  custom_domain_id: 1,
  way_in: wayIn,
  state,
  reason:
    state === 'reachable'
      ? ''
      : `${host} points at an address Temps does not know.`,
  action: null,
  network: null,
  nothing_set_up: false,
  findings: [],
})

const domain = (id: number, host: string, environmentId: number | null) => ({
  id,
  domain: host,
  project_id: 7,
  status: 'active',
  created_at: 0,
  updated_at: 0,
  environment:
    environmentId === null ? null : { id: environmentId, slug: 'production' },
  redirect_to: id === 1 ? 'https://www.example.test' : null,
  status_code: id === 1 ? 301 : null,
})

function renderList(
  addresses: ReturnType<typeof address>[],
  runs: { id: number; state: string; address: string }[] = [],
  domains?: ReturnType<typeof domain>[],
  environmentsDown = false
) {
  mockApi({
    'GET /projects/7/environments': () =>
      environmentsDown
        ? jsonResponse(
            { title: 'Unavailable', detail: 'Database is down' },
            503
          )
        : jsonResponse([{ id: 3, name: 'production', slug: 'production' }]),
    'GET /projects/7/environments/3/reachability': () =>
      jsonResponse({
        state: 'reachable',
        summary: 'Public at app.example.test',
        addresses,
      }),
    'GET /projects/7/environments/3/reachability/runs': () =>
      jsonResponse(runs),
  })
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <EnvironmentReachabilityList
          projectId={7}
          projectSlug="example"
          domains={domains as never}
          domainMenu={(d) => (
            <button type="button">Actions for {d.domain}</button>
          )}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('every address is one row under its environment, without a summary', async () => {
  const view = renderList([
    address('app.example.test', 'reachable', 'cdn:cloudflare'),
    address('example.temps.test', 'reachable', 'direct:control-plane'),
  ])

  await view.findByRole('link', { name: 'Make another address reachable' })
  expect(view.getByRole('heading', { name: 'production' })).toBeTruthy()
  expect(view.getByRole('link', { name: 'app.example.test' })).toBeTruthy()
  expect(view.getByRole('link', { name: 'example.temps.test' })).toBeTruthy()
  expect(view.getAllByRole('listitem')).toHaveLength(2)
  // The rows say it; the environment does not repeat it.
  expect(view.queryByText('Public at app.example.test')).toBeNull()
})

test('a custom domain appears once, on its address, with its actions and redirect', async () => {
  const view = renderList(
    [address('app.example.test', 'reachable', 'cdn:cloudflare')],
    [],
    [domain(1, 'app.example.test', 3), domain(2, 'old.example.test', null)]
  )

  await view.findByText('Redirects to https://www.example.test (301)')
  expect(view.getAllByText('app.example.test')).toHaveLength(1)
  expect(
    view.getByRole('button', { name: 'Actions for app.example.test' })
  ).toBeTruthy()
  // A domain on no environment of this project still appears, once.
  const other = view.getByRole('region', { name: 'Other domains' })
  expect(other.textContent).toContain('old.example.test')
  expect(view.getAllByText('old.example.test')).toHaveLength(1)
})

test('an address that needs attention shows its reason', async () => {
  const view = renderList([
    address('app.example.test', 'unknown', null),
    address('example.temps.test', 'reachable', 'direct:control-plane'),
  ])

  await view.findByText('Unknown')
  expect(
    view.getByText('app.example.test points at an address Temps does not know.')
  ).toBeTruthy()
  // Nothing on record is not the same as no way in.
  expect(view.queryByText(/No way in yet/)).toBeNull()
  expect(view.getByText('Anyone · This server')).toBeTruthy()
})

test('only a setup that did not end well is mentioned', async () => {
  const failed = renderList(
    [address('app.example.test', 'reachable', 'direct:control-plane')],
    [{ id: 9, state: 'failed', address: 'other.example.test' }]
  )
  await waitFor(() =>
    expect(failed.getByText(/Setup for other.example.test/)).toBeTruthy()
  )
  expect(failed.getByRole('link', { name: 'Open it' })).toBeTruthy()
  cleanup()

  const done = renderList(
    [address('app.example.test', 'reachable', 'direct:control-plane')],
    [{ id: 8, state: 'succeeded', address: 'app.example.test' }]
  )
  await done.findByRole('link', { name: 'app.example.test' })
  expect(done.queryByText(/Setup/)).toBeNull()
})

test('a failed refresh of the environments keeps the rows and says so', async () => {
  let environmentsDown = false
  mockApi({
    'GET /projects/7/environments': () =>
      environmentsDown
        ? jsonResponse(
            { title: 'Unavailable', detail: 'Database is down' },
            503
          )
        : jsonResponse([{ id: 3, name: 'production', slug: 'production' }]),
    'GET /projects/7/environments/3/reachability': () =>
      jsonResponse({
        state: 'reachable',
        summary: '',
        addresses: [
          address('app.example.test', 'reachable', 'direct:control-plane'),
        ],
      }),
    'GET /projects/7/environments/3/reachability/runs': () => jsonResponse([]),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <EnvironmentReachabilityList projectId={7} projectSlug="example" />
      </MemoryRouter>
    </QueryClientProvider>
  )
  await view.findByRole('link', { name: 'app.example.test' })

  environmentsDown = true
  await act(() => client.refetchQueries())

  await view.findByText(/Database is down/)
  expect(view.getByRole('link', { name: 'app.example.test' })).toBeTruthy()
})

test('environments that cannot load still list every domain with its actions', async () => {
  const view = renderList(
    [],
    [],
    [domain(1, 'app.example.test', 3), domain(2, 'other.example.test', null)],
    true
  )

  await view.findByText(/Database is down/)
  expect(view.getByRole('heading', { name: 'Domains' })).toBeTruthy()
  expect(
    view.getByRole('button', { name: 'Actions for app.example.test' })
  ).toBeTruthy()
  expect(
    view.getByRole('button', { name: 'Actions for other.example.test' })
  ).toBeTruthy()
})

test('a domain that moved to another environment is listed only where it is now', async () => {
  // The check for environment 3 still names domain 1, which is now on an
  // environment this project does not list.
  const view = renderList(
    [address('app.example.test', 'reachable', 'direct:control-plane')],
    [],
    [domain(1, 'app.example.test', 9)]
  )

  await view.findByRole('heading', { name: 'Other domains' })
  await waitFor(() =>
    expect(view.queryByText('No address yet', { exact: false })).toBeTruthy()
  )
  expect(view.getAllByRole('link', { name: 'app.example.test' })).toHaveLength(
    1
  )
})
