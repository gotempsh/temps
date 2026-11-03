// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
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

function renderList(
  addresses: ReturnType<typeof address>[],
  runs: { id: number; state: string; address: string }[] = []
) {
  mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([{ id: 3, name: 'production', slug: 'production' }]),
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
        <EnvironmentReachabilityList projectId={7} projectSlug="example" />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('an environment with every address reachable is one row, addresses included', async () => {
  const view = renderList([
    address('app.example.test', 'reachable', 'cdn:cloudflare'),
    address('example.temps.test', 'reachable', 'direct:control-plane'),
  ])

  const link = await view.findByRole('link', {
    name: 'Make another address reachable',
  })
  expect(link).toBeTruthy()
  expect(view.getByRole('link', { name: 'app.example.test' })).toBeTruthy()
  expect(view.getByRole('link', { name: 'example.temps.test' })).toBeTruthy()
  // One state for the environment, not one per address and no address list.
  expect(view.getAllByText('Reachable')).toHaveLength(1)
  expect(view.queryByRole('list')).toBeNull()
  expect(view.queryByText('Public at app.example.test')).toBeNull()
})

test('an address that needs attention keeps the full card and its reason', async () => {
  const view = renderList([
    address('app.example.test', 'unknown', null),
    address('example.temps.test', 'reachable', 'direct:control-plane'),
  ])

  await view.findByRole('list')
  expect(view.getByText('Unknown')).toBeTruthy()
  expect(
    view.getByText('app.example.test points at an address Temps does not know.')
  ).toBeTruthy()
  expect(view.getByText('Public at app.example.test')).toBeTruthy()
  // Nothing on record is not the same as no way in.
  expect(view.queryByText(/No way in yet/)).toBeNull()
  expect(view.getByText('Anyone · This server')).toBeTruthy()
})

test('a setup that did not end well keeps the full card', async () => {
  const view = renderList(
    [address('app.example.test', 'reachable', 'direct:control-plane')],
    [{ id: 9, state: 'failed', address: 'other.example.test' }]
  )

  await waitFor(() => expect(view.getByText(/Last setup/)).toBeTruthy())
  expect(view.getByRole('list')).toBeTruthy()
})
