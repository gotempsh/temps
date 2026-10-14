// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectGroupResponse } from '@/api/client'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { PresetContext } from '@/contexts/PresetContext-shared'
import { ConnectRepositoryPanel } from './ConnectRepositoryPanel'

installBrowserWindow()
afterEach(cleanup)

const GROUP: ProjectGroupResponse = {
  id: 7,
  name: 'Shop',
  slug: 'shop',
  description: null,
  projects: [],
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

const NO_CONNECTIONS = {
  'GET /git-connections': () =>
    jsonResponse({ connections: [], page: 1, per_page: 20, total_count: 0 }),
  'GET /git-providers': () => jsonResponse([]),
}

const PRESETS = {
  presets: [],
  isLoading: false,
  error: null,
  getPresetBySlug: () => undefined,
}

function panel(
  props: {
    group?: ProjectGroupResponse
    onConnected?: () => void
    onCancel?: () => void
    client?: QueryClient
  } = {}
) {
  const client =
    props.client ??
    new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return (
    <QueryClientProvider client={client}>
      <PresetContext.Provider value={PRESETS}>
        <MemoryRouter>
          <ConnectRepositoryPanel
            group={props.group ?? GROUP}
            title="Connect a repository"
            description="Connect once."
            onCancel={props.onCancel ?? (() => {})}
            onConnected={props.onConnected ?? (() => {})}
          />
        </MemoryRouter>
      </PresetContext.Provider>
    </QueryClientProvider>
  )
}

function urlInput(): HTMLInputElement {
  return within(document.body).getByLabelText(
    'Repository URL'
  ) as HTMLInputElement
}

async function typeUrl(value: string) {
  await act(async () => {
    fireEvent.change(urlInput(), { target: { value } })
  })
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30))
  })
}

function saveButton() {
  return within(document.body).getByRole('button', {
    name: /save repository/i,
  }) as HTMLButtonElement
}

/** The template marks a not-yet-savable button `aria-disabled`, never `disabled`. */
function saveDisabled() {
  return saveButton().getAttribute('aria-disabled') === 'true'
}

async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30))
  })
}

test('with no git connection the public tab is shown and starts empty', async () => {
  const api = mockApi(NO_CONNECTIONS)
  render(panel())
  await settle()

  expect(urlInput().value).toBe('')
  expect(saveDisabled()).toBe(true)
  api.restore()
})

test('a malformed URL is explained as it is typed', async () => {
  const api = mockApi(NO_CONNECTIONS)
  render(panel())
  await settle()

  await typeUrl('definitely not a url')

  // Inline on the field and in the template's summary above the save bar.
  expect(
    within(document.body).getAllByText(
      'Enter a repository URL like https://github.com/owner/repo'
    ).length
  ).toBe(2)
  expect(saveDisabled()).toBe(true)
  api.restore()
})

test('a repository outside github.com and gitlab.com is refused', async () => {
  const api = mockApi(NO_CONNECTIONS)
  render(panel())
  await settle()

  await typeUrl('https://git.example.test/team/app')

  expect(
    within(document.body).getAllByText(/Only public repositories on github.com/)
      .length
  ).toBe(2)
  expect(saveDisabled()).toBe(true)
  api.restore()
})

test('saves a looked-up public repository on its default branch', async () => {
  const bodies: Record<string, unknown>[] = []
  const api = mockApi({
    ...NO_CONNECTIONS,
    'GET /git/public/github/acme/shop': () =>
      jsonResponse({
        default_branch: 'trunk',
        description: 'Storefront',
        forks: 0,
        full_name: 'acme/shop',
        name: 'shop',
        owner: 'acme',
      }),
    'PUT /project-groups/7/repository': async (request) => {
      bodies.push((await request.json()) as Record<string, unknown>)
      return jsonResponse({ group: GROUP, services: [] })
    },
  })
  render(panel())
  await settle()

  await typeUrl('https://github.com/acme/shop')
  expect(saveDisabled()).toBe(false)

  await act(async () => {
    fireEvent.click(saveButton())
  })
  await settle()

  expect(bodies).toEqual([
    {
      public_provider: 'github',
      repo_owner: 'acme',
      repo_name: 'shop',
      main_branch: 'trunk',
    },
  ])
  api.restore()
})

test('shows the server error when saving fails', async () => {
  const api = mockApi({
    ...NO_CONNECTIONS,
    'GET /git/public/github/acme/shop': () =>
      jsonResponse({
        default_branch: 'main',
        forks: 0,
        full_name: 'acme/shop',
        name: 'shop',
        owner: 'acme',
      }),
    'PUT /project-groups/7/repository': () =>
      jsonResponse({ title: 'Bad', detail: 'Repository is unreachable' }, 502),
  })
  render(panel())
  await settle()
  await typeUrl('https://github.com/acme/shop')

  await act(async () => {
    fireEvent.click(saveButton())
  })
  await settle()

  const alerts = within(document.body)
    .getAllByRole('alert')
    .map((alert) => alert.textContent ?? '')
  expect(
    alerts.some((text) => text.includes('Repository is unreachable'))
  ).toBe(true)
  api.restore()
})

test('with no git connection it onboards instead of showing an empty picker', async () => {
  const api = mockApi(NO_CONNECTIONS)
  render(panel())
  await settle()
  // The public tab is the fallback, and the onboarding is one click away.
  // Radix tabs activate on pointer down.
  await act(async () => {
    fireEvent.mouseDown(
      within(document.body).getByRole('tab', { name: 'Git connection' })
    )
  })

  expect(
    await within(document.body).findByText('No git provider is connected yet')
  ).toBeTruthy()
  const link = within(document.body).getByRole('link', {
    name: /connect a git provider/i,
  })
  expect(link.getAttribute('href')).toBe('/git-providers/add')
  api.restore()
})

test('shows skeletons while the git connections load', async () => {
  const api = mockApi({
    'GET /git-connections': () => new Promise<Response>(() => {}),
    'GET /git-providers': () => jsonResponse([]),
  })
  render(panel())
  await settle()

  expect(
    document.body.querySelector('[aria-label="Loading repositories"]')
  ).toBeTruthy()
  expect(saveDisabled()).toBe(true)
  api.restore()
})

test('cancel leaves without saving', async () => {
  const api = mockApi(NO_CONNECTIONS)
  let cancelled = 0
  render(panel({ onCancel: () => (cancelled += 1) }))
  await settle()

  await act(async () => {
    fireEvent.click(
      within(document.body).getByRole('button', { name: 'Cancel' })
    )
  })

  expect(cancelled).toBe(1)
  expect(api.called('PUT', '/project-groups/7/repository')).toBe(false)
  api.restore()
})

const CONNECTION = {
  id: 11,
  provider_id: 3,
  account_name: 'acme',
  account_type: 'organization',
  is_active: true,
  is_expired: false,
  syncing: false,
  synced_repository_count: 2,
  has_authenticated_credentials: true,
  health_status: 'healthy',
  consecutive_health_failures: 0,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

function repo(id: number, name: string) {
  return {
    id,
    name,
    owner: 'acme',
    full_name: `acme/${name}`,
    private: id % 2 === 0,
    default_branch: 'main',
    git_provider_connection_id: CONNECTION.id,
    created_at: '2026-01-01T00:00:00Z',
    pushed_at: '2026-01-02T00:00:00Z',
    updated_at: '2026-01-02T00:00:00Z',
  }
}

const WITH_CONNECTION = {
  'GET /git-connections': () =>
    jsonResponse({
      connections: [CONNECTION],
      page: 1,
      per_page: 20,
      total_count: 1,
    }),
  'GET /git-providers': () =>
    jsonResponse([{ id: 3, name: 'GitHub', provider_type: 'github' }]),
  'GET /git-connections/11/repositories': () =>
    jsonResponse({
      repositories: [repo(1, 'storefront'), repo(2, 'admin-console')],
      total_count: 2,
    }),
  'GET /repositories/acme/admin-console/branches': () =>
    jsonResponse({ branches: [{ name: 'main', is_default: true }] }),
}

test('lists the connection repositories with their full names', async () => {
  const api = mockApi(WITH_CONNECTION)
  render(panel())
  await settle()

  const body = within(document.body)
  await waitFor(() => expect(body.getByText('storefront')).toBeTruthy())
  expect(body.getByText('admin-console')).toBeTruthy()
  expect(body.getByRole('combobox', { name: 'Git connection' })).toBeTruthy()
  expect(saveDisabled()).toBe(true)
  api.restore()
})

test('saves the picked repository on its default branch', async () => {
  const bodies: Record<string, unknown>[] = []
  const api = mockApi({
    ...WITH_CONNECTION,
    'PUT /project-groups/7/repository': async (request) => {
      bodies.push((await request.json()) as Record<string, unknown>)
      return jsonResponse({ group: GROUP, services: [] })
    },
  })
  let connected = 0
  render(panel({ onConnected: () => (connected += 1) }))
  await settle()

  await act(async () => {
    fireEvent.click(
      within(document.body).getByText('admin-console').closest('button')!
    )
  })
  await settle()
  expect(saveDisabled()).toBe(false)

  await act(async () => {
    fireEvent.click(saveButton())
  })
  await settle()

  expect(bodies).toEqual([
    {
      git_provider_connection_id: 11,
      repo_owner: 'acme',
      repo_name: 'admin-console',
      main_branch: 'main',
    },
  ])
  expect(connected).toBe(1)
  api.restore()
})

test('a service that could not follow keeps the page open and is named', async () => {
  const api = mockApi({
    ...NO_CONNECTIONS,
    'GET /git/public/github/acme/shop': () =>
      jsonResponse({
        default_branch: 'main',
        forks: 0,
        full_name: 'acme/shop',
        name: 'shop',
        owner: 'acme',
      }),
    'PUT /project-groups/7/repository': () =>
      jsonResponse({
        group: GROUP,
        services: [
          { project_id: 5, name: 'storefront', error: 'Branch is protected' },
        ],
      }),
  })
  let connected = 0
  render(panel({ onConnected: () => (connected += 1) }))
  await settle()
  await typeUrl('https://github.com/acme/shop')

  await act(async () => {
    fireEvent.click(saveButton())
  })
  await settle()

  expect(connected).toBe(0)
  const alerts = within(document.body)
    .getAllByRole('alert')
    .map((alert) => alert.textContent ?? '')
  expect(alerts.some((text) => text.includes('Branch is protected'))).toBe(true)
  expect(
    within(document.body).getByRole('button', { name: 'Retry' })
  ).toBeTruthy()
  api.restore()
})

test('paging the list after picking a repository does not save it', async () => {
  const bodies: unknown[] = []
  const api = mockApi({
    ...WITH_CONNECTION,
    'GET /git-connections/11/repositories': () =>
      jsonResponse({
        repositories: [repo(1, 'storefront'), repo(2, 'admin-console')],
        total_count: 40,
      }),
    'PUT /project-groups/7/repository': async (request) => {
      bodies.push(await request.json())
      return jsonResponse({ group: GROUP, services: [] })
    },
  })
  let connected = 0
  render(panel({ onConnected: () => (connected += 1) }))
  const body = within(document.body)
  await waitFor(() => expect(body.getByText('admin-console')).toBeTruthy())

  await act(async () => {
    fireEvent.click(body.getByText('admin-console').closest('button')!)
  })
  await act(async () => {
    fireEvent.click(body.getByRole('button', { name: 'Next' }))
  })
  await settle()

  expect(bodies).toEqual([])
  expect(connected).toBe(0)
  api.restore()
})

test('a pick is dropped when its connection stops being the active one', async () => {
  const second = { ...CONNECTION, id: 12, account_name: 'acme-labs' }
  let connectionList = [CONNECTION, second]
  const api = mockApi({
    ...WITH_CONNECTION,
    'GET /git-connections': () =>
      jsonResponse({
        connections: connectionList,
        page: 1,
        per_page: 20,
        total_count: connectionList.length,
      }),
    'GET /git-connections/12/repositories': () =>
      jsonResponse({ repositories: [], total_count: 0 }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  render(panel({ client }))
  const body = within(document.body)
  await waitFor(() => expect(body.getByText('admin-console')).toBeTruthy())
  await act(async () => {
    fireEvent.click(body.getByText('admin-console').closest('button')!)
  })
  await settle()
  expect(saveDisabled()).toBe(false)

  // The connection is deactivated elsewhere; the list refreshes.
  connectionList = [second]
  await act(async () => {
    await client.invalidateQueries({ queryKey: [{ _id: 'listConnections' }] })
  })
  await settle()

  expect(saveDisabled()).toBe(true)
  api.restore()
})

test('Enter in a field never saves the repository', async () => {
  const api = mockApi({
    ...NO_CONNECTIONS,
    'GET /git/public/github/acme/shop': () =>
      jsonResponse({
        default_branch: 'main',
        forks: 0,
        full_name: 'acme/shop',
        name: 'shop',
        owner: 'acme',
      }),
  })
  render(panel())
  await settle()
  await typeUrl('https://github.com/acme/shop')
  expect(saveDisabled()).toBe(false)

  let allowed = true
  await act(async () => {
    allowed = fireEvent.keyDown(urlInput(), { key: 'Enter' })
  })
  await settle()

  expect(allowed).toBe(false)
  expect(api.called('PUT', '/project-groups/7/repository')).toBe(false)
  api.restore()
})
