// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { MonitoringSettingsPage } from './MonitoringSettingsPage'

installBrowserWindow()

const STORED = {
  preview_domain: 'apps.example.test',
  external_url: null,
  monitoring: {
    store: 'timescale_db',
    retention_raw_days: 7,
    retention_hourly_days: 90,
    retention_daily_years: 2,
  },
  analytics_retention: {
    events_days: 45,
    error_events_days: 14,
    visitor_days: 120,
    unreferenced_ip_days: 30,
  },
}

interface Recorded {
  puts: { analytics_retention: Record<string, number | null> }[]
}

const originalFetch = globalThis.fetch
const originalBaseUrl = client.getConfig().baseUrl
let recorded: Recorded

beforeEach(() => {
  recorded = { puts: [] }
  client.setConfig({ baseUrl: 'http://console.example.test/api' })
  globalThis.fetch = (async (input: Request | string | URL) => {
    const request =
      input instanceof Request ? input : new Request(String(input))
    if (request.method === 'PUT') {
      recorded.puts.push(JSON.parse(await request.text()))
      return new Response(JSON.stringify({ message: 'ok' }), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      })
    }
    return new Response(JSON.stringify(STORED), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  }) as unknown as typeof fetch
})

afterEach(() => {
  cleanup()
  globalThis.fetch = originalFetch
  client.setConfig({ baseUrl: originalBaseUrl })
})

function mount(settings: object = STORED) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  queryClient.setQueryData(['platform-settings'], settings)
  return render(
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Example Admin',
              username: 'example',
              avatar_url: '',
              mfa_enabled: false,
              role: 'admin',
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <MonitoringSettingsPage />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function field(view: ReturnType<typeof mount>, name: string) {
  return view.container.querySelector(
    `#analytics-retention-${name}`
  ) as HTMLInputElement
}

test('missing analytics settings default to 365 days and can be adjusted', async () => {
  const legacy = { ...STORED, analytics_retention: undefined }
  globalThis.fetch = (async (input: Request | string | URL) => {
    const request =
      input instanceof Request ? input : new Request(String(input))
    if (request.method === 'PUT') {
      recorded.puts.push(JSON.parse(await request.text()))
    }
    return new Response(JSON.stringify(legacy), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  }) as unknown as typeof fetch
  const view = mount(legacy)
  await waitFor(() => expect(field(view, 'events_days').value).toBe('365'))
  expect(view.getByText(/Defaults to 365 days/)).toBeTruthy()

  fireEvent.change(field(view, 'events_days'), { target: { value: '90' } })
  fireEvent.click(await view.findByText('Save Changes'))
  await waitFor(() => expect(recorded.puts).toHaveLength(1))
  expect(recorded.puts[0].analytics_retention.events_days).toBe(90)
})

test('stored retention windows fill the form, and an empty optional one is saved as keep', async () => {
  const view = mount()
  await waitFor(() => expect(field(view, 'visitor_days').value).toBe('120'))
  expect(field(view, 'events_days').value).toBe('45')
  expect(field(view, 'error_events_days').value).toBe('14')

  fireEvent.change(field(view, 'error_events_days'), { target: { value: '' } })
  fireEvent.change(field(view, 'events_days'), { target: { value: '60' } })
  fireEvent.click(await view.findByText('Save Changes'))

  await waitFor(() => expect(recorded.puts).toHaveLength(1))
  expect(recorded.puts[0].analytics_retention).toEqual({
    events_days: 60,
    error_events_days: null,
    visitor_days: 120,
    unreferenced_ip_days: 30,
  })
})

test('an invalid window is reported and nothing is sent', async () => {
  const view = mount()
  await waitFor(() => expect(field(view, 'events_days').value).toBe('45'))

  // Submitted directly: the browser's own min/max check would otherwise stop
  // the submit before the form's validation runs.
  const form = view.container.querySelector('form') as HTMLFormElement
  fireEvent.change(field(view, 'events_days'), { target: { value: '0' } })
  fireEvent.submit(form)

  await view.findByText('Must be a whole number of days from 1 to 3650')
  expect(recorded.puts).toHaveLength(0)

  // The window that must have a value cannot be cleared either.
  fireEvent.change(field(view, 'events_days'), { target: { value: '' } })
  fireEvent.submit(form)
  await view.findByText('A retention window is required')
  expect(recorded.puts).toHaveLength(0)
})

test('retention explains both event stores, asynchronous expiry and visitor cleanup scope', () => {
  const view = mount()
  expect(
    view.getByText(/Applies to both PostgreSQL and ClickHouse/).textContent
  ).toContain('asynchronous background merges')
  expect(
    view.getByText(/With ClickHouse enabled, their events are removed/)
      .textContent
  ).toContain('a ClickHouse failure leaves the visitor for retry')
  expect(
    view.getByText(/There is no manual visitor-delete action/).textContent
  ).toContain('this is not erasure of all visitor data')
})
