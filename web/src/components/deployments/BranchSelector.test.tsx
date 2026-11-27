// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { BranchSelector } from './BranchSelector'

installBrowserWindow()
afterEach(cleanup)

const branch = (name: string, is_default = false) => ({
  name,
  commit_sha: name.padEnd(40, '0'),
  protected: false,
  is_default,
})

function renderSelector(
  routes: Parameters<typeof mockApi>[0],
  value = 'main',
  defaultBranchProp: string | undefined = 'main'
) {
  mockApi(routes)
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <BranchSelector
          repoOwner="temps"
          repoName="example"
          connectionId={7}
          defaultBranch={defaultBranchProp}
          value={value}
          onChange={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the default badge is shown only for the branch the server marked default', async () => {
  // The server's default is `release`; the caller's `defaultBranch` prop says
  // `main`, which is merely the environment's branch. The prop must not
  // decide the badge.
  const view = renderSelector({
    'GET /repositories/temps/example/branches': () =>
      jsonResponse({
        branches: [branch('main'), branch('release', true)],
        truncated: false,
      }),
  })

  await waitFor(() => expect(view.getByRole('combobox')).toBeTruthy())
  // `main` is selected and is not the server's default, so the trigger shows
  // no badge.
  expect(view.queryByText('default')).toBeNull()
})

test('a server-marked default wears the badge when it is the selected branch', async () => {
  const view = renderSelector({
    'GET /repositories/temps/example/branches': () =>
      jsonResponse({
        branches: [branch('main', true), branch('topic')],
        truncated: false,
      }),
  })

  await waitFor(() => expect(view.getByText('default')).toBeTruthy())
})

test('a selected branch past the cap is confirmed by the exact lookup, badge included', async () => {
  const view = renderSelector({
    'GET /repositories/temps/example/branches': () =>
      jsonResponse({ branches: [branch('topic-1')], truncated: true }),
    'GET /repositories/temps/example/branch': () =>
      jsonResponse({
        name: 'main',
        commit_sha: 'a'.repeat(40),
        protected: true,
        is_default: true,
      }),
  })

  // The lookup says `main` is the repository's default, so the badge belongs
  // to it even though the capped listing never returned it.
  await waitFor(() => expect(view.getByText('default')).toBeTruthy())
})

test('the exact lookup is not spent on a branch the listing already contains', async () => {
  const api = mockApi({
    'GET /repositories/temps/example/branches': () =>
      jsonResponse({
        branches: [branch('main', true), branch('topic')],
        truncated: false,
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <BranchSelector
          repoOwner="temps"
          repoName="example"
          connectionId={7}
          value="main"
          onChange={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )

  await waitFor(() => expect(view.getByText('default')).toBeTruthy())
  expect(api.called('GET', '/repositories/temps/example/branch')).toBe(false)
})

test('refreshing branches asks the exact lookup again for a branch past the cap', async () => {
  // The branch moves from commit A to commit B between the first lookup and
  // the refresh; the listing still never reaches it.
  let head = 'a'.repeat(40)
  let exactLookups = 0
  const details: (string | undefined)[] = []
  mockApi({
    'GET /repositories/temps/example/branches': () =>
      jsonResponse({ branches: [branch('topic-1')], truncated: true }),
    'GET /repositories/temps/example/branch': () => {
      exactLookups += 1
      return jsonResponse({
        name: 'main',
        commit_sha: head,
        protected: false,
        is_default: false,
      })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <BranchSelector
          repoOwner="temps"
          repoName="example"
          connectionId={7}
          value="main"
          onChange={() => {}}
          onBranchDetailsLoaded={(branches) =>
            details.push(branches.find((b) => b.name === 'main')?.commit_sha)
          }
        />
      </MemoryRouter>
    </QueryClientProvider>
  )

  await waitFor(() => expect(details[details.length - 1]).toBe('a'.repeat(40)))
  expect(exactLookups).toBe(1)

  head = 'b'.repeat(40)
  fireEvent.click(view.getByTitle('Refresh branches'))

  // The dialog is handed the branch's new head, not the one from before.
  await waitFor(() => expect(details[details.length - 1]).toBe('b'.repeat(40)))
  expect(exactLookups).toBe(2)
})
