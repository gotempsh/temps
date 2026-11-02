// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
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
    })
) {
  mockApi({
    'GET /projects/7/environments': () => jsonResponse([environment]),
    'GET /projects/7/env-vars': () => jsonResponse(variables),
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
