// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { NotificationRouteForm } from './NotificationRouteForm'

installBrowserWindow()
afterEach(cleanup)

const PROVIDERS = [
  { id: 1, name: 'Ops channel', provider_type: 'slack', config: {} },
  { id: 2, name: 'On-call email', provider_type: 'email', config: {} },
]

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={['/settings/notifications/routes/new']}>
          <Routes>
            <Route
              path="/settings/notifications/routes/new"
              element={<NotificationRouteForm />}
            />
          </Routes>
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
}

test('an empty submit marks the name and the destinations invalid at once', async () => {
  const api = mockApi({
    'GET /notification-providers': () => jsonResponse(PROVIDERS),
  })
  const view = renderPage()
  await view.findByText('Ops channel')

  fireEvent.click(view.getByRole('button', { name: 'Create Route' }))

  const name = view.container.querySelector<HTMLInputElement>('#route-name')!
  expect(name.getAttribute('aria-invalid')).toBe('true')
  const nameMessage = view.getByText('Route name is required')
  expect(name.getAttribute('aria-describedby')).toBe(nameMessage.id)

  const providerMessage = view.getByText('Select at least one provider')
  for (const checkbox of view.getAllByRole('checkbox')) {
    expect(checkbox.getAttribute('aria-invalid')).toBe('true')
    expect(checkbox.getAttribute('aria-describedby')).toBe(providerMessage.id)
  }
  expect(api.called('POST', '/notification-routes')).toBe(false)
  api.restore()
})

test('focus moves to the first invalid field as it appears on screen', async () => {
  const api = mockApi({
    'GET /notification-providers': () => jsonResponse(PROVIDERS),
  })
  const view = renderPage()
  await view.findByText('Ops channel')

  fireEvent.click(view.getByRole('button', { name: 'Create Route' }))
  // Destinations render above the name, so they are first.
  expect(document.activeElement?.getAttribute('role')).toBe('checkbox')

  fireEvent.click(view.getAllByRole('checkbox')[0])
  fireEvent.click(view.getByRole('button', { name: 'Create Route' }))
  expect(document.activeElement?.id).toBe('route-name')
  api.restore()
})

test('editing a field withdraws only its own message', async () => {
  const api = mockApi({
    'GET /notification-providers': () => jsonResponse(PROVIDERS),
  })
  const view = renderPage()
  await view.findByText('Ops channel')
  fireEvent.click(view.getByRole('button', { name: 'Create Route' }))

  const name = view.container.querySelector<HTMLInputElement>('#route-name')!
  fireEvent.change(name, { target: { value: 'Critical' } })

  expect(name.getAttribute('aria-invalid')).toBeNull()
  expect(view.queryByText('Route name is required')).toBeNull()
  expect(view.getByText('Select at least one provider')).toBeTruthy()
  api.restore()
})

test('a failed provider read is explained and focus still lands on a field', async () => {
  const api = mockApi({
    'GET /notification-providers': () =>
      jsonResponse({ title: 'Internal Server Error', detail: 'boom' }, 500),
  })
  const view = renderPage()
  await view.findByText('Notification providers unavailable')

  fireEvent.click(view.getByRole('button', { name: 'Create Route' }))

  expect(view.getByText('Select at least one provider')).toBeTruthy()
  expect(document.activeElement?.id).toBe('route-providers-failure')

  fireEvent.change(
    view.container.querySelector<HTMLInputElement>('#route-name')!,
    { target: { value: 'Critical' } }
  )
  fireEvent.click(view.getByRole('button', { name: 'Create Route' }))
  expect(document.activeElement?.id).toBe('route-providers-failure')
  api.restore()
})

test('retrying a failed provider read reloads without submitting the form', async () => {
  const api = mockApi({
    'GET /notification-providers': () =>
      jsonResponse({ title: 'Internal Server Error', detail: 'boom' }, 500),
  })
  const view = renderPage()
  await view.findByText('Notification providers unavailable')

  fireEvent.click(view.getByRole('button', { name: 'Retry' }))

  // A submit would have validated the form and reported the missing fields.
  expect(view.queryByText('Route name is required')).toBeNull()
  expect(view.queryByText('Select at least one provider')).toBeNull()
  expect(api.called('POST', '/notification-routes')).toBe(false)
  api.restore()
})
