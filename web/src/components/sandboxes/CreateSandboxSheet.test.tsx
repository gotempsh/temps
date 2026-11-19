// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { listSandboxesQueryKey } from '@/api/client/@tanstack/react-query.gen'
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
    // Lets the sheet poll the create's steps while it waits.
    creation_id: expect.stringMatching(/^[A-Za-z0-9_-]{8,64}$/),
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

test('a slow create explains the wait and can be closed', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    // Never answers, like a first create that is still building its image.
    'POST /v1/sandboxes': () => new Promise<Response>(() => {}),
  })
  const view = renderSheet()

  fireEvent.click(await view.findByRole('button', { name: 'Create sandbox' }))

  await view.findByText(/You can close this panel/)
  // Steps load as skeleton rows until the first poll answers.
  expect(
    view
      .getByRole('list', { name: 'Sandbox creation steps' })
      .getAttribute('aria-busy')
  ).toBe('true')
  // The footer button and the sheet's own close control both stay usable.
  const closers = view.getAllByRole('button', { name: 'Close' })
  expect(closers.length).toBeGreaterThan(0)
  for (const closer of closers) {
    expect((closer as HTMLButtonElement).disabled).toBe(false)
  }
  api.restore()
})

const CREATION_ID = '00000000-0000-4000-8000-000000000001'

/** Makes the sheet's next `creation_id` predictable, so its poll can be mocked. */
function fixCreationId() {
  const original = crypto.randomUUID.bind(crypto)
  crypto.randomUUID = () => CREATION_ID
  return () => {
    crypto.randomUUID = original
  }
}

function creationStep(
  kind: string,
  title: string,
  status: string,
  extra: Record<string, unknown> = {}
) {
  return {
    kind,
    title,
    status,
    detail: null,
    error: null,
    started_at: status === 'pending' ? null : '2026-01-01T00:00:00Z',
    finished_at:
      status === 'done' || status === 'failed' ? '2026-01-01T00:00:02Z' : null,
    bytes_done: null,
    bytes_total: null,
    ...extra,
  }
}

test('a running create shows its steps live, with the download progress', async () => {
  const restoreId = fixCreationId()
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    'POST /v1/sandboxes': () => new Promise<Response>(() => {}),
    [`GET /v1/sandboxes/creations/${CREATION_ID}`]: () =>
      jsonResponse({
        creation_id: CREATION_ID,
        sandbox_id: 'sbx_new1',
        status: 'running',
        started_at: '2026-01-01T00:00:00Z',
        finished_at: null,
        error: null,
        steps: [
          creationStep('check_request', 'Check the request', 'done'),
          creationStep('pull_image', 'Pull the sandbox image', 'running', {
            detail: 'downloading',
            bytes_done: 1_000_000_000,
            bytes_total: 4_000_000_000,
          }),
          creationStep('ready', 'Ready', 'pending'),
        ],
      }),
  })
  const view = renderSheet()

  fireEvent.click(await view.findByRole('button', { name: 'Create sandbox' }))

  await view.findByText('1.0 GB of 4.0 GB')
  expect(view.getByText('Check the request')).toBeTruthy()
  expect(view.getByText('2.0s')).toBeTruthy()
  api.restore()
  restoreId()
})

test('a failed create shows which step failed and why', async () => {
  const restoreId = fixCreationId()
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    'POST /v1/sandboxes': () =>
      jsonResponse(
        { title: 'Sandbox Creation Failed', detail: 'pulling image failed' },
        500
      ),
    [`GET /v1/sandboxes/creations/${CREATION_ID}`]: () =>
      jsonResponse({
        creation_id: CREATION_ID,
        sandbox_id: null,
        status: 'failed',
        started_at: '2026-01-01T00:00:00Z',
        finished_at: '2026-01-01T00:00:02Z',
        error: 'pulling image failed',
        steps: [
          creationStep('check_request', 'Check the request', 'done'),
          creationStep('pull_image', 'Pull the sandbox image', 'failed', {
            error: "pulling image 'img:1' failed: no space left on device",
          }),
          creationStep('ready', 'Ready', 'pending'),
        ],
      }),
  })
  const view = renderSheet()

  fireEvent.click(await view.findByRole('button', { name: 'Create sandbox' }))

  await view.findByText(/no space left on device/)
  expect(view.getByText(/Failed at “Pull the sandbox image”/)).toBeTruthy()
  expect(view.queryByText('detail page')).toBeNull()
  api.restore()
  restoreId()
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

test('creating invalidates every cached page of the sandbox list', async () => {
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT),
    'POST /v1/sandboxes': () => jsonResponse(CREATED, 201),
  })
  const client = newClient()
  const pageOne = listSandboxesQueryKey({ query: { page: 1, page_size: 20 } })
  const pageTwo = listSandboxesQueryKey({ query: { page: 2, page_size: 20 } })
  client.setQueryData(pageOne, { sandboxes: [] })
  client.setQueryData(pageTwo, { sandboxes: [] })
  const view = render(sheetTree(client, true, true))

  fireEvent.click(await view.findByRole('button', { name: 'Create sandbox' }))
  await view.findByText('detail page')

  expect(client.getQueryState(pageOne)?.isInvalidated).toBe(true)
  expect(client.getQueryState(pageTwo)?.isInvalidated).toBe(true)
  api.restore()
})

const PLACEMENT_WITH_BACKENDS = {
  allowed_node_ids: null,
  nodes: [
    {
      ...PLACEMENT.nodes[0],
      backends: [
        { backend: 'docker', available: true, reason: null },
        { backend: 'firecracker', available: true, reason: null },
        {
          backend: 'microsandbox',
          available: false,
          reason: 'run `temps microsandbox setup` on the server host',
        },
      ],
    },
  ],
}

test('offers the backends the node runs and explains the missing ones', async () => {
  let body: unknown
  const api = mockApi({
    'GET /v1/sandboxes/placement': () => jsonResponse(PLACEMENT_WITH_BACKENDS),
    'POST /v1/sandboxes': async (request) => {
      body = await request.json()
      return jsonResponse(CREATED, 201)
    },
  })
  const view = renderSheet()

  // The unavailable backend stays visible, disabled, with the server's fix.
  await view.findByText(/temps microsandbox setup/)
  const microsandbox = view.baseElement.querySelector('#backend-microsandbox')
  expect(microsandbox?.hasAttribute('disabled')).toBe(true)

  const firecracker = view.baseElement.querySelector('#backend-firecracker')
  expect(firecracker?.hasAttribute('disabled')).toBe(false)
  fireEvent.click(firecracker as Element)
  fireEvent.click(view.getByRole('button', { name: 'Create sandbox' }))

  await view.findByText('detail page')
  expect(body).toEqual({
    lifecycle: 'ephemeral',
    backend: 'firecracker',
    creation_id: expect.any(String),
  })
  api.restore()
})
