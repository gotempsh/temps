// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { LogIngestKeys, LogIngestKeysBody } from './LogIngestKeys'

installBrowserWindow()
afterEach(cleanup)

const renderBody = (projectId: number | undefined) =>
  renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <LogIngestKeysBody projectId={projectId} enabled={false} />
    </QueryClientProvider>
  )

test('without a service the body shows nothing: the dialog lists services', () => {
  expect(renderBody(undefined)).toBe('')
})

test('with a service the body offers to create a key', () => {
  const html = renderBody(4)
  expect(html).toContain('Create key')
})

const services = [
  { id: 4, name: 'api', slug: 'api', source_type: 'git' },
  { id: 5, name: 'web', slug: 'web', source_type: 'git' },
]

function providers(children: React.ReactNode, client: QueryClient) {
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>{children}</MemoryRouter>
    </QueryClientProvider>
  )
}

function mountDialog(projectId: number | undefined) {
  const api = mockApi({
    'GET /projects': () =>
      jsonResponse({ projects: services, total: 2, page: 1, per_page: 100 }),
    'GET /projects/4': () => jsonResponse(services[0]),
    'GET /projects/5': () => jsonResponse(services[1]),
    'GET /projects/4/logs/ingest-keys': () => jsonResponse({ data: [] }),
    'GET /projects/5/logs/ingest-keys': () => jsonResponse({ data: [] }),
  })
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <LogIngestKeys projectId={projectId} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, view }
}

test('the action says what it adds, and asks nothing until opened', () => {
  const { api, view } = mountDialog(undefined)
  expect(view.getByRole('button', { name: 'Add log source' })).toBeTruthy()
  expect(api.calls).toEqual([])
})

test('without a service the dialog lists them, and one click reaches the form', async () => {
  const { view } = mountDialog(undefined)
  fireEvent.click(view.getByRole('button', { name: 'Add log source' }))

  const dialog = await view.findByRole('dialog')
  // What a log source is, before anything is asked.
  expect(dialog.textContent).toContain('it did not deploy')
  expect(within(dialog).queryByRole('button', { name: 'Create key' })).toBe(
    null
  )
  // Choosing is a click, never an instruction to type a filter.
  expect(dialog.textContent).not.toContain('project:')

  const list = await within(dialog).findByRole('list', { name: 'Services' })
  fireEvent.click(within(list).getByRole('button', { name: 'web' }))

  expect(
    await within(dialog).findByRole('button', { name: 'Create key' })
  ).toBeTruthy()
  expect(await within(dialog).findByText('web')).toBeTruthy()
})

test("with the page's service it names it, and another can be chosen", async () => {
  const { view } = mountDialog(4)
  fireEvent.click(view.getByRole('button', { name: 'Add log source' }))

  const dialog = await view.findByRole('dialog')
  expect(await within(dialog).findByText('api')).toBeTruthy()
  expect(
    within(dialog).getByRole('button', { name: 'Create key' })
  ).toBeTruthy()

  fireEvent.click(within(dialog).getByRole('button', { name: 'Change' }))
  expect(
    await within(dialog).findByRole('list', { name: 'Services' })
  ).toBeTruthy()
  expect(within(dialog).queryByRole('button', { name: 'Create key' })).toBe(
    null
  )
})

test("opened again after the page moves to another service, it starts from the page's", async () => {
  const { view } = mountDialog(undefined)
  fireEvent.click(view.getByRole('button', { name: 'Add log source' }))
  let dialog = await view.findByRole('dialog')
  const list = await within(dialog).findByRole('list', { name: 'Services' })
  fireEvent.click(within(list).getByRole('button', { name: 'api' }))
  expect(await within(dialog).findByText('api')).toBeTruthy()
  fireEvent.keyDown(dialog, { key: 'Escape' })
  await waitFor(() => expect(view.queryByRole('dialog')).toBeNull())

  // The page is now filtered to web.
  view.rerender(
    providers(
      <LogIngestKeys projectId={5} />,
      new QueryClient({ defaultOptions: { queries: { retry: false } } })
    )
  )
  fireEvent.click(view.getByRole('button', { name: 'Add log source' }))
  dialog = await view.findByRole('dialog')
  expect(await within(dialog).findByText('web')).toBeTruthy()
  expect(within(dialog).queryByText('api')).toBeNull()
})
