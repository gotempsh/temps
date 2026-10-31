// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import type { ApiRule } from './api'
import { EdgeRulesPage } from './EdgeRulesPage'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 1, slug: 'web' } as ProjectResponse

function apiRule(id: number, name: string): ApiRule {
  return {
    id,
    revision: 1,
    name,
    enabled: true,
    mode: 'enforce',
    environment_id: null,
    expression: {
      combinator: 'all',
      children: [
        {
          kind: 'condition',
          field: 'path',
          op: 'starts_with',
          values: [`/${name.toLowerCase()}`],
        },
      ],
    },
    action: { type: 'block', status: 403 },
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    hits: 0,
  }
}

const RULES = [apiRule(1, 'Admin'), apiRule(2, 'Login'), apiRule(3, 'Wp')]

/** A response the test releases when it is ready. */
function deferred() {
  let release: (response: Response) => void = () => {}
  const promise = new Promise<Response>((resolve) => {
    release = resolve
  })
  return { promise, release }
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/projects/web/settings/edge-rules']}>
        <BreadcrumbProvider>
          <EdgeRulesPage project={project} />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const button = (name: string) =>
  screen.getByRole('button', { name }) as HTMLButtonElement

async function openMenu(trigger: HTMLElement) {
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
}

test('announces loading from a status region that stays mounted', async () => {
  const list = deferred()
  mockApi({
    'GET /projects/1/edge-rules': () => list.promise,
    'GET /projects/1/environments': () => jsonResponse([]),
  })
  renderPage()
  const status = screen.getByRole('status')
  expect(status.textContent).toBe('Loading edge rules')

  await act(async () => list.release(jsonResponse({ rules: RULES })))
  await screen.findByRole('table')
  // Same node, now empty: a live region that is replaced is never announced.
  expect(screen.getByRole('status')).toBe(status)
  expect(status.textContent).toBe('')
})

test('every move button waits while a reorder is in flight', async () => {
  const reorder = deferred()
  mockApi({
    'GET /projects/1/edge-rules': () => jsonResponse({ rules: RULES }),
    'GET /projects/1/environments': () => jsonResponse([]),
    'PUT /projects/1/edge-rules/order': () => reorder.promise,
  })
  renderPage()
  await screen.findByRole('table')
  expect(button('Move Login up').disabled).toBe(false)

  await act(async () => {
    fireEvent.click(button('Move Admin down'))
  })
  // Indexes shift when the first move lands, so no other move may start.
  await waitFor(() => expect(button('Move Login up').disabled).toBe(true))
  expect(button('Move Wp up').disabled).toBe(true)
  expect(button('Move Login down').disabled).toBe(true)

  await act(async () =>
    reorder.release(jsonResponse({ rules: [RULES[1], RULES[0], RULES[2]] }))
  )
  await waitFor(() => expect(button('Move Login down').disabled).toBe(false))
})

test('a pending delete keeps both dialog buttons disabled', async () => {
  const remove = deferred()
  const api = mockApi({
    'GET /projects/1/edge-rules': () => jsonResponse({ rules: RULES }),
    'GET /projects/1/environments': () => jsonResponse([]),
    'DELETE /projects/1/edge-rules/2': () => remove.promise,
  })
  renderPage()
  await screen.findByRole('table')

  await openMenu(button('More actions for Login'))
  await act(async () => {
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Delete' }))
  })
  const dialog = await screen.findByRole('alertdialog')
  expect(dialog.textContent).toContain('Login')

  const confirm = Array.from(dialog.querySelectorAll('button')).find(
    (b) => b.textContent === 'Delete'
  )
  if (!confirm) throw new Error('no Delete button in the dialog')
  await act(async () => {
    fireEvent.click(confirm)
  })
  await waitFor(() =>
    expect(
      api.calls.some(
        (c) => c.method === 'DELETE' && c.path === '/projects/1/edge-rules/2'
      )
    ).toBe(true)
  )

  const buttons = Array.from(dialog.querySelectorAll('button'))
  const cancel = buttons.find((b) => b.textContent === 'Cancel')
  expect(cancel?.disabled).toBe(true)
  expect(buttons.filter((b) => !b.disabled)).toEqual([])

  await act(async () => remove.release(new Response(null, { status: 204 })))
  await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
})

test('cancelling a delete puts focus back on the row menu button', async () => {
  mockApi({
    'GET /projects/1/edge-rules': () => jsonResponse({ rules: RULES }),
    'GET /projects/1/environments': () => jsonResponse([]),
  })
  renderPage()
  await screen.findByRole('table')
  const trigger = button('More actions for Login')

  await openMenu(trigger)
  await act(async () => {
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Delete' }))
  })
  const dialog = await screen.findByRole('alertdialog')
  const cancel = Array.from(dialog.querySelectorAll('button')).find(
    (b) => b.textContent === 'Cancel'
  )
  if (!cancel) throw new Error('no Cancel button in the dialog')
  await act(async () => {
    fireEvent.click(cancel)
  })
  await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
  // The menu item that opened the dialog no longer exists.
  await waitFor(() => expect(document.activeElement).toBe(trigger))
})
