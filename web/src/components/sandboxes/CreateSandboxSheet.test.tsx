// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CreateSandboxSheet } from './CreateSandboxSheet'

installBrowserWindow()
afterEach(cleanup)

const PLACEMENT = {
  allowed_node_ids: null,
  nodes: [
    {
      id: 0,
      name: 'control-plane',
      is_control_plane: true,
      status: 'active',
      allowed: true,
      eligible: true,
      live_sandboxes: 2,
    },
  ],
}

const CREATED = {
  routes: [],
  sandbox: { id: 'sbx_new1', name: 'scratch' },
}

function sheetTree(client: QueryClient, open: boolean, available: boolean) {
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/sandboxes']}>
        <Routes>
          <Route
            path="/sandboxes"
            element={
              <CreateSandboxSheet
                open={open}
                onOpenChange={() => {}}
                available={available}
              />
            }
          />
          <Route path="/sandboxes/:id" element={<p>detail page</p>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function newClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
}

function renderSheet(available = true) {
  return render(sheetTree(newClient(), true, available))
}

test('creating sends only the filled fields and opens the new sandbox', async () => {
  let body: unknown
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    'POST /v1/sandboxes': async (request) => {
      body = await request.json()
      return jsonResponse(CREATED, 201)
    },
  })
  const view = renderSheet()

  fireEvent.change(await view.findByLabelText('Name'), {
    target: { value: 'scratch' },
  })
  fireEvent.change(view.getByLabelText('Ports'), {
    target: { value: '3000, 8080' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Create sandbox' }))

  await view.findByText('detail page')
  expect(body).toEqual({
    lifecycle: 'ephemeral',
    name: 'scratch',
    ports: [3000, 8080],
  })
  api.restore()
})

test('an invalid value is reported and nothing is sent', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    'POST /v1/sandboxes': () => jsonResponse(CREATED, 201),
  })
  const view = renderSheet()

  fireEvent.change(await view.findByLabelText('Preview password'), {
    target: { value: 'short' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Create sandbox' }))

  await view.findByText(/Preview password must be 8-256 characters/)
  expect(api.called('POST', '/v1/sandboxes')).toBe(false)
  api.restore()
})

test('a refused create keeps the sheet open and shows the reason', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    'POST /v1/sandboxes': () =>
      jsonResponse(
        {
          title: 'Node unavailable',
          detail: 'Node worker-9 is offline; the sandbox was not placed.',
        },
        422
      ),
  })
  const view = renderSheet()

  fireEvent.click(await view.findByRole('button', { name: 'Create sandbox' }))

  await view.findByText(/Node worker-9 is offline/)
  expect(view.queryByText('detail page')).toBeNull()
  api.restore()
})

test('says so, and cannot submit, when sandboxes are unavailable', async () => {
  const api = mockApi({})
  const view = renderSheet(false)

  await view.findByText(/Sandboxes are not available on this server/)
  const submit = view.getByRole('button', {
    name: 'Create sandbox',
  }) as HTMLButtonElement
  expect(submit.disabled).toBe(true)
  expect(api.called('GET', '/v1/sandboxes/placement')).toBe(false)
  api.restore()
})

test('loads node placement so a node can be chosen explicitly', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
  })
  const view = renderSheet()
  await waitFor(() =>
    expect(api.called('GET', '/v1/sandboxes/placement')).toBe(true)
  )
  expect(view.getByLabelText('Node')).toBeTruthy()
  api.restore()
})

test("reopening the sheet does not show the previous attempt's error", async () => {
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    'POST /v1/sandboxes': () =>
      jsonResponse(
        { title: 'Node unavailable', detail: 'Node worker-9 is offline.' },
        422
      ),
  })
  const client = newClient()
  const view = render(sheetTree(client, true, true))

  fireEvent.click(await view.findByRole('button', { name: 'Create sandbox' }))
  await view.findByText(/Node worker-9 is offline/)

  view.rerender(sheetTree(client, false, true))
  view.rerender(sheetTree(client, true, true))

  await waitFor(() =>
    expect(view.queryByText(/Node worker-9 is offline/)).toBeNull()
  )
  api.restore()
})
