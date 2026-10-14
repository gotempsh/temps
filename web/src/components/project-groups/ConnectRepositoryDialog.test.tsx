// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectGroupResponse } from '@/api/client'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { ConnectRepositoryDialog } from './ConnectRepositoryDialog'

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

function dialog(open: boolean) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ConnectRepositoryDialog
          group={GROUP}
          open={open}
          onOpenChange={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function urlInput(): HTMLInputElement {
  const input = document.body.querySelector<HTMLInputElement>(
    '#public-repository-url'
  )
  if (!input) throw new Error('the repository URL field is not shown')
  return input
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

async function settle() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30))
  })
}

test('with no git connection the public tab is shown and starts empty', async () => {
  const api = mockApi(NO_CONNECTIONS)
  render(dialog(true))
  await settle()

  expect(urlInput().value).toBe('')
  expect(saveButton().disabled).toBe(true)
  api.restore()
})

test('a malformed URL is explained as it is typed', async () => {
  const api = mockApi(NO_CONNECTIONS)
  render(dialog(true))
  await settle()

  await typeUrl('definitely not a url')

  expect(
    within(document.body).getByText(
      'Enter a repository URL like https://github.com/owner/repo'
    )
  ).toBeTruthy()
  expect(saveButton().disabled).toBe(true)
  api.restore()
})

test('a repository outside github.com and gitlab.com is refused', async () => {
  const api = mockApi(NO_CONNECTIONS)
  render(dialog(true))
  await settle()

  await typeUrl('https://git.example.test/team/app')

  expect(
    within(document.body).getByText(/Only public repositories on github.com/)
  ).toBeTruthy()
  expect(saveButton().disabled).toBe(true)
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
  render(dialog(true))
  await settle()

  await typeUrl('https://github.com/acme/shop')
  expect(saveButton().disabled).toBe(false)

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
  render(dialog(true))
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

test('reopening starts from an empty form', async () => {
  const api = mockApi(NO_CONNECTIONS)
  const view = render(dialog(true))
  await settle()
  await typeUrl('https://github.com/acme/shop')
  expect(urlInput().value).toBe('https://github.com/acme/shop')

  await act(async () => {
    view.rerender(dialog(false))
  })
  await act(async () => {
    view.rerender(dialog(true))
  })
  await settle()

  expect(urlInput().value).toBe('')
  api.restore()
})
