// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { EnvironmentVariablesSettings } from './EnvironmentVariablesSettings'

installBrowserWindow()
afterEach(cleanup)

const environment = { id: 3, name: 'production', slug: 'production' }

const variable = (id: number, key: string) => ({
  id,
  key,
  value: 'value',
  is_secret: false,
  include_in_preview: false,
  created_at: 1,
  updated_at: 1,
  environments: [
    { id: 3, name: 'production', main_url: '', current_deployment_id: null },
  ],
})

const httpCheck = (envVarId: number, status: string) => ({
  id: envVarId,
  project_id: 7,
  env_var_id: envVarId,
  secret_id: null,
  kind: 'http',
  name: 'Check',
  automatic_provider: null,
  enabled: true,
  interval_seconds: 86400,
  next_check_at: '2026-10-03T00:00:00Z',
  last_checked_at: '2026-10-02T00:00:00Z',
  result: {
    status,
    findings: [],
    checked_at: '2026-10-02T00:00:00Z',
  },
})

const variables = [
  variable(1, 'FINE_KEY'),
  variable(2, 'BROKEN_KEY'),
  variable(3, 'AGING_KEY'),
  variable(4, 'UNWATCHED_KEY'),
]

function CurrentSearch() {
  return <output data-testid="search">{useLocation().search}</output>
}

function renderPage(
  search: string,
  checks: () => Response = () =>
    jsonResponse({
      items: [
        httpCheck(1, 'healthy'),
        httpCheck(2, 'error'),
        httpCheck(3, 'warning'),
      ],
      total: 3,
    }),
  listed = variables
) {
  mockApi({
    'GET /projects/7/environments': () => jsonResponse([environment]),
    'GET /projects/7/env-vars': () => jsonResponse(listed),
    'GET /projects/7/http-checks': checks,
    'GET /projects/7/env-vars/resolved': () =>
      jsonResponse([
        {
          key: 'SERVICE_URL',
          value_preview: 'postgres://***',
          include_in_preview: true,
          environments: [],
          source: {
            type: 'integration',
            service: { id: 1, name: 'main-db', service_type: 'postgres' },
          },
        },
      ]),
  })
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[`/environment-variables${search}`]}>
        <EnvironmentVariablesSettings
          project={
            { id: 7, slug: 'example', name: 'Example' } as ProjectResponse
          }
        />
        <CurrentSearch />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

type View = ReturnType<typeof renderPage>

const rowHidden = (view: View, key: string) =>
  view
    .getByLabelText(`View ${key} details`)
    .closest('tr')
    ?.classList.contains('hidden')

const shown = (view: View) =>
  variables
    .map((row) => row.key)
    .filter((key) => !rowHidden(view, key))
    .sort()

test('the checks filter lists only variables with that result', async () => {
  const view = renderPage('?check=error')

  await waitFor(() => expect(shown(view)).toEqual(['BROKEN_KEY']))
})

test('a warning and an unverified variable land in their own filters', async () => {
  const warning = renderPage('?check=warning')
  await waitFor(() => expect(shown(warning)).toEqual(['AGING_KEY']))
  cleanup()

  // No checks reads as "No issues" on the row, but nothing vouches for it.
  const healthy = renderPage('?check=healthy')
  await waitFor(() => expect(shown(healthy)).toEqual(['FINE_KEY']))
  cleanup()

  const unchecked = renderPage('?check=unchecked')
  await waitFor(() => expect(shown(unchecked)).toEqual(['UNWATCHED_KEY']))
})

test('without a filter every variable is listed', async () => {
  const view = renderPage('')

  await view.findByLabelText('View BROKEN_KEY details')
  expect(shown(view)).toHaveLength(4)
})

test('selecting all takes only what the filter shows, so a bulk delete cannot reach hidden rows', async () => {
  const view = renderPage('?check=error')
  await waitFor(() => expect(shown(view)).toEqual(['BROKEN_KEY']))

  view
    .getByRole('checkbox', { name: 'Select all environment variables' })
    .click()

  await view.findByRole('button', { name: 'Delete 1 selected' })
})

test('check results that fail to load leave the list whole and say why', async () => {
  const view = renderPage('?check=error', () =>
    jsonResponse({ title: 'Unavailable' }, 500)
  )

  await view.findByText(/cannot be filtered by check/)
  expect(shown(view)).toHaveLength(4)
})

test('an integration value has no checks, so only "Not checked" lists it', async () => {
  const unchecked = renderPage('?check=unchecked')
  await waitFor(() =>
    expect(unchecked.container.textContent).toContain('SERVICE_URL')
  )
  cleanup()

  const failing = renderPage('?check=error')
  await waitFor(() => expect(shown(failing)).toEqual(['BROKEN_KEY']))
  expect(failing.container.textContent).not.toContain('SERVICE_URL')
})

test("a row's actions show an icon beside their names", async () => {
  const view = renderPage('')
  const trigger = await view.findByRole('button', {
    name: 'Actions for FINE_KEY',
  })

  fireEvent.keyDown(trigger, { key: 'Enter' })

  for (const name of ['Edit', 'Delete']) {
    const item = await view.findByRole('menuitem', { name })
    expect(item.querySelector('svg[aria-hidden="true"]')).not.toBeNull()
  }
})

test('a project with only integration values says the filter matched nothing, not that it has no variables', async () => {
  const view = renderPage('?check=error', undefined, [])

  await view.findByText('No variables match these filters.')
  expect(view.queryByText('No environment variables')).toBeNull()

  fireEvent.click(view.getByRole('button', { name: 'Clear filters' }))
  await waitFor(() =>
    expect(view.container.textContent).toContain('SERVICE_URL')
  )
})

test('a filter from a link can be cleared while check results are unavailable', async () => {
  const view = renderPage('?check=error', () =>
    jsonResponse({ title: 'Unavailable' }, 500)
  )
  await view.findByText(/cannot be filtered by check/)

  const filters = view.getByRole('button', { name: 'Filters' })
  if (filters.getAttribute('aria-expanded') !== 'true') fireEvent.click(filters)
  const select = await view.findByRole('combobox', { name: 'Checks' })
  fireEvent.keyDown(select, { key: 'Enter' })

  // Choosing a result to filter by still needs the results.
  const failing = await view.findByRole('option', { name: 'Not healthy' })
  expect(failing.getAttribute('aria-disabled')).toBe('true')

  fireEvent.keyDown(view.getByRole('option', { name: 'All checks' }), {
    key: 'Enter',
  })
  await waitFor(() =>
    expect(view.getByTestId('search').textContent).not.toContain('check=')
  )
})

test('without a filter to clear, the selector waits for check results', async () => {
  const view = renderPage('', () => jsonResponse({ title: 'Unavailable' }, 500))
  await view.findByLabelText('View BROKEN_KEY details')

  const filters = view.getByRole('button', { name: 'Filters' })
  if (filters.getAttribute('aria-expanded') !== 'true') fireEvent.click(filters)
  await waitFor(() =>
    expect(
      view.getByRole('combobox', { name: 'Checks' }).hasAttribute('disabled')
    ).toBe(true)
  )
})
