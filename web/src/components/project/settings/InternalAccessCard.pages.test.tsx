// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type {
  InternalAccessGrantResponse,
  InternalAccessSandboxGrantResponse,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { InternalAccessCard } from './InternalAccessCard'

installBrowserWindow()
afterEach(cleanup)

function projectGrant(
  callerId: number,
  id = callerId
): InternalAccessGrantResponse {
  return {
    id,
    target_project_id: 1,
    target_project_name: 'Orders API',
    target_project_slug: 'orders-api',
    target_environment_id: null,
    target_environment_name: null,
    caller_project_id: callerId,
    caller_project_name: `Caller ${callerId}`,
    caller_project_slug: `caller-${callerId}`,
    caller_environment_id: null,
    caller_environment_name: null,
    description: null,
    created_by: 9,
    created_at: '2026-11-09T10:00:00Z',
  }
}

function sandboxGrant(sandboxId: string): InternalAccessSandboxGrantResponse {
  return {
    id: 70,
    target_project_id: 1,
    target_project_name: 'Orders API',
    target_project_slug: 'orders-api',
    target_environment_id: null,
    target_environment_name: null,
    sandbox_id: sandboxId,
    sandbox_name: 'e2e caller',
    sandbox_status: 'running',
    description: null,
    created_by: 9,
    created_at: '2026-11-12T10:00:00Z',
  }
}

/** Every project but the card's own, `count` of them, paged as the API pages them. */
function projectsRoute(count: number) {
  const all = [
    { id: 1, name: 'Orders API', slug: 'orders-api' },
    ...Array.from({ length: count }, (_, i) => ({
      id: 100 + i + 1,
      name: `Caller ${100 + i + 1}`,
      slug: `caller-${100 + i + 1}`,
    })),
  ]
  return (request: Request) => {
    const url = new URL(request.url)
    const page = Number(url.searchParams.get('page') ?? '1')
    const perPage = Number(url.searchParams.get('per_page') ?? '20')
    return jsonResponse({
      projects: all.slice((page - 1) * perPage, page * perPage),
      total: all.length,
      page,
      per_page: perPage,
    })
  }
}

function list<T>(items: T[]) {
  return jsonResponse({ items, total: items.length, page: 1, page_size: 100 })
}

function sandboxesRoute() {
  return () =>
    jsonResponse({
      pagination: { count: 2 },
      sandboxes: [
        { id: 'sbx_old', name: 'old box', status: 'stopped' },
        { id: 'sbx_e2e', name: 'e2e caller', status: 'running' },
      ],
    })
}

function renderCard() {
  render(
    <MemoryRouter>
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <InternalAccessCard
          projectId={1}
          projectName="Orders API"
          projectSlug="orders-api"
        />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

test('one click allows a project with the defaults, then it reads Allowed and can be removed', async () => {
  let grants: InternalAccessGrantResponse[] = []
  const bodies: unknown[] = []
  const api = mockApi({
    'GET /projects': projectsRoute(2),
    'GET /v1/sandboxes': sandboxesRoute(),
    'GET /projects/1/internal-access': () => list(grants),
    'GET /projects/1/internal-access/sandboxes': () => list([]),
    'POST /projects/1/internal-access': async (request) => {
      const body = (await request.json()) as { caller_project_id: number }
      bodies.push(body)
      grants = [projectGrant(body.caller_project_id, 9)]
      return jsonResponse(grants[0], 201)
    },
    'DELETE /projects/1/internal-access/9': () => {
      grants = []
      return new Response(null, { status: 204 })
    },
  })
  renderCard()

  const allow = await screen.findByRole('button', { name: 'Allow Caller 101' })
  await waitFor(() => expect(allow.textContent).toBe('Allow'))
  fireEvent.click(allow)

  // Every environment of the caller, the whole project, no reason.
  await waitFor(() => expect(bodies).toEqual([{ caller_project_id: 101 }]))
  const remove = await screen.findByRole('button', {
    name: 'Remove internal access for Caller 101',
  })
  const row = remove.closest('li') as HTMLElement
  expect(within(row).getByText('Allowed')).toBeTruthy()
  expect(within(row).getByText('all environments')).toBeTruthy()
  // The other project is untouched.
  expect(screen.getByRole('button', { name: 'Allow Caller 102' })).toBeTruthy()

  fireEvent.click(remove)
  const dialog = await screen.findByRole('alertdialog')
  fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }))
  await waitFor(() =>
    expect(api.called('DELETE', '/projects/1/internal-access/9')).toBe(true)
  )
  await screen.findByRole('button', { name: 'Allow Caller 101' })
  api.restore()
})

test('the project list pages at 20', async () => {
  const api = mockApi({
    'GET /projects': projectsRoute(30),
    'GET /v1/sandboxes': sandboxesRoute(),
    'GET /projects/1/internal-access': () => list([]),
    'GET /projects/1/internal-access/sandboxes': () => list([]),
  })
  renderCard()

  await screen.findByText('Caller 101')
  expect(screen.queryByText('Caller 125')).toBeNull()
  fireEvent.click(screen.getAllByLabelText('Next page')[0])
  await screen.findByText('Caller 125')
  expect(screen.queryByText('Caller 101')).toBeNull()
  api.restore()
})

test('a failed grants lookup pauses Allow until Retry succeeds', async () => {
  let failing = true
  const api = mockApi({
    'GET /projects': projectsRoute(1),
    'GET /v1/sandboxes': sandboxesRoute(),
    'GET /projects/1/internal-access': () =>
      failing
        ? jsonResponse({ title: 'Unavailable', detail: 'Database down' }, 503)
        : list([projectGrant(101)]),
    'GET /projects/1/internal-access/sandboxes': () => list([]),
  })
  renderCard()

  await screen.findByText('Could not check which projects are allowed')
  const paused = screen.getByRole('button', { name: 'Allow Caller 101' })
  expect(paused.textContent).toBe('Paused')
  expect((paused as HTMLButtonElement).disabled).toBe(true)

  failing = false
  fireEvent.click(screen.getByRole('button', { name: /Try again/ }))
  // Caller 101 was allowed all along: it now says so, and never offered Allow.
  await screen.findByRole('button', {
    name: 'Remove internal access for Caller 101',
  })
  expect(
    screen.queryByText('Could not check which projects are allowed')
  ).toBeNull()
  expect(api.called('POST', '/projects/1/internal-access')).toBe(false)
  api.restore()
})

test('one click allows a sandbox with only its id', async () => {
  let grants: InternalAccessSandboxGrantResponse[] = []
  const bodies: unknown[] = []
  const api = mockApi({
    'GET /projects': projectsRoute(0),
    'GET /v1/sandboxes': sandboxesRoute(),
    'GET /projects/1/internal-access': () => list([]),
    'GET /projects/1/internal-access/sandboxes': () => list(grants),
    'POST /projects/1/internal-access/sandboxes': async (request) => {
      const body = (await request.json()) as { sandbox_id: string }
      bodies.push(body)
      grants = [sandboxGrant(body.sandbox_id)]
      return jsonResponse(grants[0], 201)
    },
  })
  renderCard()

  const allow = await screen.findByRole('button', { name: 'Allow e2e caller' })
  await waitFor(() => expect(allow.textContent).toBe('Allow'))
  fireEvent.click(allow)
  await waitFor(() => expect(bodies).toEqual([{ sandbox_id: 'sbx_e2e' }]))
  await screen.findByRole('button', {
    name: 'Remove internal access for e2e caller',
  })
  api.restore()
})

test('a refused Allow says why in the row', async () => {
  const api = mockApi({
    'GET /projects': projectsRoute(1),
    'GET /v1/sandboxes': sandboxesRoute(),
    'GET /projects/1/internal-access': () => list([]),
    'GET /projects/1/internal-access/sandboxes': () => list([]),
    'POST /projects/1/internal-access': () =>
      jsonResponse(
        {
          title: 'Insufficient Permissions',
          detail: 'This operation requires the projects:write permission',
        },
        403
      ),
  })
  renderCard()

  const allow = await screen.findByRole('button', { name: 'Allow Caller 101' })
  await waitFor(() => expect(allow.textContent).toBe('Allow'))
  fireEvent.click(allow)
  const row = allow.closest('li') as HTMLElement
  await waitFor(() =>
    expect(within(row).getByRole('alert').textContent).toContain(
      'requires the projects:write permission'
    )
  )
  api.restore()
})

test('Limit… allows with a narrower scope and a reason', async () => {
  const bodies: unknown[] = []
  const api = mockApi({
    'GET /projects': projectsRoute(1),
    'GET /v1/sandboxes': sandboxesRoute(),
    'GET /projects/1/internal-access': () => list([]),
    'GET /projects/1/internal-access/sandboxes': () => list([]),
    'GET /projects/1/environments': () => jsonResponse([]),
    'GET /projects/101/environments': () => jsonResponse([]),
    'POST /projects/1/internal-access': async (request) => {
      bodies.push(await request.json())
      return jsonResponse(projectGrant(101, 9), 201)
    },
  })
  renderCard()

  const limit = await screen.findByRole('button', {
    name: 'Allow Caller 101 with limits',
  })
  await waitFor(() => expect((limit as HTMLButtonElement).disabled).toBe(false))
  fireEvent.click(limit)
  const form = await screen.findByRole('form', {
    name: 'Allow Caller 101 with limits',
  })
  fireEvent.change(within(form).getByLabelText('Reason (optional)'), {
    target: { value: '  the worker places orders ' },
  })
  fireEvent.click(within(form).getByRole('button', { name: 'Allow' }))
  await waitFor(() =>
    expect(bodies).toEqual([
      {
        caller_project_id: 101,
        caller_environment_id: null,
        target_environment_id: null,
        description: 'the worker places orders',
      },
    ])
  )
  api.restore()
})
