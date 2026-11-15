// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import type { ProjectResponse } from '@/api/client'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CreateFunnel } from './CreateFunnel'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = { id: 3, slug: 'shop' } as ProjectResponse

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/projects/shop/analytics/funnels/new']}>
        <BreadcrumbProvider>
          <Routes>
            <Route
              path="/projects/shop/analytics/funnels/new"
              element={<CreateFunnel project={PROJECT} />}
            />
            <Route
              path="/projects/shop/analytics/funnels/:id"
              element={<p>Funnel opened</p>}
            />
          </Routes>
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the form stays disabled between a create and the redirect, so it cannot create twice', async () => {
  let creates = 0
  const api = mockApi({
    'GET /projects/3/events/unique': () => jsonResponse({ events: [] }),
    'POST /projects/3/funnels/preview': () =>
      jsonResponse({
        funnel_id: 0,
        funnel_name: 'Preview',
        average_completion_time_seconds: 0,
        overall_conversion_rate: 0,
        step_conversions: [],
        total_entries: 0,
      }),
    'POST /projects/3/funnels': () => {
      creates += 1
      return jsonResponse({ funnel_id: 41 }, 201)
    },
  })
  const view = renderPage()

  await act(async () => {
    fireEvent.change(view.getByLabelText('Funnel Name *'), {
      target: { value: 'Checkout' },
    })
  })
  await act(async () => {
    fireEvent.click(view.getByRole('combobox'))
  })
  const eventInput = await view.findByPlaceholderText(
    'Search events or type custom name...'
  )
  await act(async () => {
    fireEvent.change(eventInput, { target: { value: 'checkout_started' } })
  })
  await act(async () => {
    fireEvent.keyDown(eventInput, { key: 'Escape' })
  })

  const submit = view.getByRole('button', { name: 'Create Funnel' })
  await act(async () => {
    fireEvent.click(submit)
  })
  expect(await view.findByText('Funnel created successfully!')).toBeTruthy()

  // The success message is up and the redirect has not happened yet: the
  // submit button must not accept a second create.
  const saving = view.getByRole('button', { name: 'Saving...' })
  expect((saving as HTMLButtonElement).disabled).toBe(true)
  await act(async () => {
    fireEvent.click(saving)
  })
  expect(creates).toBe(1)

  expect(
    await view.findByText('Funnel opened', undefined, { timeout: 3000 })
  ).toBeTruthy()
  expect(creates).toBe(1)
  api.restore()
})
