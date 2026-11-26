// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { PageNamedByChromeContext } from '@temps-sdk/ds'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { AiOnboarding } from './AiOnboarding'

installBrowserWindow()
afterEach(() => {
  cleanup()
  restoreAllMockApis()
})

const STATUS = 'GET /api-keys/ai-harness/status'

/** The steps' headings as a screen reader hears them. */
const stepTitles = (container: HTMLElement) =>
  [...container.querySelectorAll('ol h2')].map((h) => h.textContent)

function mount(routes: Parameters<typeof mockApi>[0]) {
  const api = mockApi(routes)
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AuthContext.Provider
        value={{
          user: {
            id: 1,
            name: 'Example User',
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
          <MemoryRouter initialEntries={['/setup/ai']}>
            {/* The Connect tabs above the page already name it. */}
            <PageNamedByChromeContext.Provider value={true}>
              <AiOnboarding />
            </PageNamedByChromeContext.Provider>
          </MemoryRouter>
        </BreadcrumbProvider>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
  return { api, view }
}

test('without a key: the state, three numbered steps and one way to create the key', async () => {
  const { view } = mount({
    [STATUS]: () => jsonResponse({ status: 'missing' }),
  })

  expect(await view.findByText('Not connected')).toBeTruthy()
  const steps = within(
    view.getByRole('list', { name: 'Steps to connect an agent' })
  )
  expect(
    steps.getAllByRole('heading', { level: 2 }).map((h) => h.textContent)
  ).toEqual([
    'Create an admin key',
    'Add Temps to your agent',
    'Check the connection',
  ])
  // The page used to offer the same action twice, in a hero and in a step.
  expect(
    view.getAllByRole('button', { name: 'Create admin key' })
  ).toHaveLength(1)
  expect(view.queryByRole('button', { name: 'Remove pending key' })).toBe(null)
})

test('the page is laid out by its shell: no padding, hero or gradient of its own', async () => {
  const { view } = mount({
    [STATUS]: () => jsonResponse({ status: 'missing' }),
  })
  await view.findByText('Not connected')

  const root = view.container.firstElementChild
  expect(root?.className).toBe('w-full min-w-0 space-y-6')
  // One page title, kept for screen readers: the tab strip prints the name.
  const titles = view.getAllByRole('heading', { level: 1 })
  expect(titles.map((h) => h.textContent)).toEqual(['Connect an agent'])
  expect(titles[0].className).toContain('sr-only')
  const html = view.container.innerHTML
  expect(html).not.toContain('gradient')
  expect(html).not.toContain('uppercase')
  expect(html).not.toContain('shadow')
})

test('waiting: says so, and offers to remove the key nothing has used', async () => {
  const { view } = mount({
    [STATUS]: () => jsonResponse({ status: 'waiting', key_id: 9 }),
  })

  expect(await view.findByText('Waiting for first request')).toBeTruthy()
  expect(view.getByRole('button', { name: 'Remove pending key' })).toBeTruthy()
  expect(view.queryByRole('button', { name: 'Create admin key' })).toBe(null)
  expect(
    view.getByRole('link', { name: 'Manage key' }).getAttribute('href')
  ).toBe('/settings/keys/9')
  expect(stepTitles(view.container)).toEqual([
    'Create an admin key, done',
    'Add Temps to your agent',
    'Check the connection',
  ])
})

test('connected: every step is done and the credential is named', async () => {
  const { view } = mount({
    [STATUS]: () =>
      jsonResponse({
        status: 'connected',
        key_id: 9,
        connected_via: {
          id: 9,
          key_prefix: 'tk_ab',
          kind: 'dedicated_key',
          last_used_at: '2026-01-01T00:00:00Z',
          name: 'Temps AI harness',
        },
      }),
  })

  expect(await view.findByText('Connected')).toBeTruthy()
  expect(view.container.textContent).toContain(
    'Authenticated with the “Temps AI harness” key'
  )
  expect(stepTitles(view.container)).toEqual([
    'Create an admin key, done',
    'Add Temps to your agent, done',
    'Check the connection, done',
  ])
  expect(view.queryByRole('button', { name: 'Remove pending key' })).toBe(null)
})

test('a key that cannot connect says why and links to it, without offering another', async () => {
  const { view } = mount({
    [STATUS]: () =>
      jsonResponse({
        status: 'missing',
        key_id: 9,
        reason: 'The key was deactivated.',
        setup_path: '/settings/keys/9',
      }),
  })

  expect(await view.findByText('Key cannot connect')).toBeTruthy()
  expect(view.container.textContent).toContain('The key was deactivated.')
  expect(view.getAllByRole('link', { name: 'Manage key' })).toHaveLength(1)
  expect(view.queryByRole('button', { name: 'Create admin key' })).toBe(null)
})

test('a status that cannot be read is not shown as missing: retry, no create', async () => {
  const { view } = mount({
    [STATUS]: () => jsonResponse({ detail: 'The database is away.' }, 500),
  })

  expect(await view.findByText('Could not check the connection')).toBeTruthy()
  expect(view.getByText('Status unknown')).toBeTruthy()
  expect(view.getByRole('button', { name: 'Retry' })).toBeTruthy()
  expect(view.queryByRole('button', { name: 'Create admin key' })).toBe(null)
  // The commands stay: they do not depend on the status.
  expect(view.container.textContent).toContain('Install the Temps skill')
})

test('prompts are a flat list, each with its own copy action', async () => {
  const { view } = mount({
    [STATUS]: () => jsonResponse({ status: 'missing' }),
  })
  await view.findByText('Not connected')

  expect(
    view.getAllByRole('heading', { level: 3 }).map((h) => h.textContent)
  ).toEqual(['Verify access', 'Create a service', 'Create PostgreSQL'])
  for (const title of [
    'Verify access',
    'Create a service',
    'Create PostgreSQL',
  ])
    expect(
      view.getByRole('button', { name: `Copy ${title} prompt` })
    ).toBeTruthy()
})
