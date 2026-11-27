// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { RedeploymentModal } from './RedeploymentModal'

installBrowserWindow()
afterEach(cleanup)

const NOT_FOUND = /was not found in the repository/
const UNVERIFIED = /could not be verified/

const project = {
  id: 7,
  slug: 'example',
  source_type: 'git',
  repo_owner: 'temps',
  repo_name: 'example',
  git_provider_connection_id: 3,
  main_branch: 'main',
} as ProjectResponse

/** Both environments deploy `main`, which is the branch under test. */
const environments = [
  {
    id: 1,
    name: 'production',
    slug: 'production',
    branch: 'main',
    is_preview: false,
    current_deployment_id: null,
  },
  {
    id: 2,
    name: 'staging',
    slug: 'staging',
    branch: 'main',
    is_preview: false,
    current_deployment_id: null,
  },
]

const branch = (name: string) => ({
  name,
  commit_sha: name.padEnd(40, '0'),
  protected: false,
  is_default: false,
})

/**
 * The dialog over a branch listing that does not contain `main`, plus
 * whatever the exact single-branch lookup answers.
 */
function renderModal(options: {
  truncated: boolean
  lookup: (request: Request) => Response
  /** Omit to leave the lookup unmocked, i.e. still in flight / 404-by-default. */
  withLookup?: boolean
  /** More routes, such as the head-commit lookup a found branch leads to. */
  extraRoutes?: Record<string, (request: Request) => Response>
}) {
  const api = mockApi({
    'GET /projects/by-slug/example': () => jsonResponse(project),
    'GET /projects/7/environments': () => jsonResponse(environments),
    'GET /repositories/temps/example': () =>
      jsonResponse({ id: 11, owner: 'temps', name: 'example' }),
    'GET /repositories/temps/example/branches': () =>
      jsonResponse({
        branches: [branch('topic-1'), branch('topic-2')],
        truncated: options.truncated,
      }),
    ...(options.withLookup === false
      ? {}
      : { 'GET /repositories/temps/example/branch': options.lookup }),
    ...options.extraRoutes,
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <RedeploymentModal
          project={project}
          isOpen
          onClose={() => {}}
          onConfirm={async () => {}}
          defaultBranch="main"
          defaultEnvironment={1}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { api, view }
}

/**
 * Pick the `staging` environment, which puts its (missing) branch in the
 * field; production is preselected, so choosing it again would change
 * nothing. Resolves with the manual-entry input the confirmed 404 shows.
 */
async function enterManualEntry(view: ReturnType<typeof render>) {
  await pickStaging(view)
  return view.findByPlaceholderText('Enter branch name manually')
}

/** Choose the `staging` environment in the dialog's picker. */
async function pickStaging(view: ReturnType<typeof render>) {
  const trigger = await waitFor(() => {
    const element = view.baseElement.querySelector<HTMLElement>('#environment')
    if (!element || element.hasAttribute('data-disabled')) {
      throw new Error('environment picker not ready')
    }
    return element
  })
  fireEvent.pointerDown(trigger, {
    button: 0,
    ctrlKey: false,
    pointerType: 'mouse',
  })
  fireEvent.click(await view.findByRole('option', { name: 'staging' }))
}

test('a branch the exact lookup finds raises no warning, however capped the list', async () => {
  const { view } = renderModal({
    truncated: true,
    lookup: () =>
      jsonResponse({
        name: 'main',
        commit_sha: 'a'.repeat(40),
        protected: false,
        is_default: true,
      }),
  })

  // The combobox renders the branch; the list simply never reached it.
  await waitFor(() => expect(view.getByRole('combobox')).toBeTruthy())
  await waitFor(() =>
    expect(view.queryByText(/Checking .main. with the Git provider/)).toBeNull()
  )
  expect(view.queryByText(NOT_FOUND)).toBeNull()
  expect(view.queryByText(UNVERIFIED)).toBeNull()
})

test('a branch the exact lookup cannot find is reported missing', async () => {
  const { view } = renderModal({
    truncated: true,
    lookup: () => jsonResponse({ title: 'Branch Not Found' }, 404),
  })

  await waitFor(() => expect(view.getByText(NOT_FOUND)).toBeTruthy())
})

test('a list the provider ran to the end is itself the answer', async () => {
  // Nothing was left unread, so the branch really is absent — and the
  // warning does not wait on a lookup that answers 404 anyway.
  const { view } = renderModal({
    truncated: false,
    lookup: () => jsonResponse({ title: 'Branch Not Found' }, 404),
  })

  await waitFor(() => expect(view.getByText(NOT_FOUND)).toBeTruthy())
})

test('a capped list plus a lookup that failed says unverified, not missing', async () => {
  const { view } = renderModal({
    truncated: true,
    lookup: () => jsonResponse({ title: 'Rate limit exceeded' }, 429),
  })

  await waitFor(() => expect(view.getByText(UNVERIFIED)).toBeTruthy())
  expect(view.queryByText(NOT_FOUND)).toBeNull()
})

test('nothing alarming is said while the lookup is still in flight', async () => {
  let release: (() => void) | undefined
  const pending = new Promise<void>((resolve) => {
    release = resolve
  })
  const { view } = renderModal({
    truncated: true,
    lookup: () =>
      pending.then(() =>
        jsonResponse({
          name: 'main',
          commit_sha: 'a'.repeat(40),
          protected: false,
          is_default: false,
        })
      ) as unknown as Response,
  })

  await waitFor(() =>
    expect(view.getByText(/Checking .main. with the Git provider/)).toBeTruthy()
  )
  expect(view.queryByText(NOT_FOUND)).toBeNull()
  expect(view.queryByText(UNVERIFIED)).toBeNull()
  release?.()
})

test('typing a replacement for a confirmed-missing branch keeps the field mounted and focused', async () => {
  const looked: string[] = []
  const { view } = renderModal({
    truncated: true,
    lookup: (request) => {
      looked.push(new URL(request.url).searchParams.get('name') ?? '')
      return jsonResponse({ title: 'Branch Not Found' }, 404)
    },
  })

  const input = await enterManualEntry(view)
  await waitFor(() => expect(view.getByText(NOT_FOUND)).toBeTruthy())
  input.focus()
  const lookupsBeforeTyping = looked.length

  fireEvent.change(input, { target: { value: 'mai' } })
  fireEvent.change(input, { target: { value: 'main-2' } })

  // The new name is pending verification, yet the field the user is typing
  // in is the same element, still focused, with no stale "not found".
  expect(view.getByPlaceholderText('Enter branch name manually')).toBe(input)
  expect(document.activeElement).toBe(input)
  expect(view.queryByText(NOT_FOUND)).toBeNull()

  // Only the name the user settled on is looked up, once.
  await act(() => new Promise((resolve) => setTimeout(resolve, 500)))
  await waitFor(() => expect(view.getByText(NOT_FOUND)).toBeTruthy())
  expect(looked.slice(lookupsBeforeTyping)).toEqual(['main-2'])
  expect(view.getByPlaceholderText('Enter branch name manually')).toBe(input)
  expect(document.activeElement).toBe(input)
})

test('a lookup that failed in manual entry can be checked again', async () => {
  let replacementAttempts = 0
  const { view } = renderModal({
    truncated: true,
    lookup: (request) => {
      const name = new URL(request.url).searchParams.get('name')
      if (name === 'main')
        return jsonResponse({ title: 'Branch Not Found' }, 404)
      replacementAttempts += 1
      return replacementAttempts === 1
        ? jsonResponse({ title: 'Rate limit exceeded' }, 429)
        : jsonResponse({
            name: 'main-2',
            commit_sha: 'c'.repeat(40),
            protected: false,
            is_default: false,
          })
    },
  })

  const input = await enterManualEntry(view)

  fireEvent.change(input, { target: { value: 'main-2' } })
  await waitFor(() => expect(view.getByText(UNVERIFIED)).toBeTruthy())

  // The combobox and its refresh control are not shown in manual entry, so
  // the alert itself offers the retry.
  fireEvent.click(view.getByRole('button', { name: 'Check again' }))
  await waitFor(() => expect(view.queryByText(UNVERIFIED)).toBeNull())
  expect(view.queryByText(NOT_FOUND)).toBeNull()
  expect(replacementAttempts).toBe(2)
  expect(view.getByPlaceholderText('Enter branch name manually')).toBe(input)
})

test('a confirmed-missing branch can be checked again once it exists upstream', async () => {
  let attempts = 0
  const { view } = renderModal({
    truncated: true,
    lookup: () => {
      attempts += 1
      return attempts === 1
        ? jsonResponse({ title: 'Branch Not Found' }, 404)
        : jsonResponse({
            name: 'main',
            commit_sha: 'd'.repeat(40),
            protected: false,
            is_default: true,
          })
    },
  })

  const input = await enterManualEntry(view)
  await waitFor(() => expect(view.getByText(NOT_FOUND)).toBeTruthy())

  // The branch was created upstream: the same name is asked again in place.
  fireEvent.click(view.getByRole('button', { name: 'Check again' }))
  await waitFor(() => expect(view.queryByText(NOT_FOUND)).toBeNull())
  expect(attempts).toBe(2)
  expect(view.getByPlaceholderText('Enter branch name manually')).toBe(input)
})

test('a branch the exact lookup finds in manual entry gets its commit preview', async () => {
  const sha = 'c'.repeat(40)
  const { view } = renderModal({
    truncated: true,
    extraRoutes: {
      [`GET /repository/11/commits/${sha}`]: () =>
        jsonResponse({
          exists: true,
          commit_sha: sha,
          commit: {
            sha,
            message: 'Add the replacement branch',
            author: 'Example Author',
            author_email: 'author@example.com',
            date: '2026-01-01T00:00:00Z',
          },
        }),
    },
    lookup: (request) =>
      new URL(request.url).searchParams.get('name') === 'main-2'
        ? jsonResponse({
            name: 'main-2',
            commit_sha: sha,
            protected: false,
            is_default: false,
          })
        : jsonResponse({ title: 'Branch Not Found' }, 404),
  })
  const input = await enterManualEntry(view)
  fireEvent.change(input, { target: { value: 'main-2' } })

  // The listing (not mounted in manual entry) never had `main-2`; the head
  // commit is the one the exact lookup returned.
  await waitFor(() => expect(view.getByText('Commit to deploy')).toBeTruthy())
  expect(view.getByText(/Add the replacement branch/)).toBeTruthy()
  expect(view.getByPlaceholderText('Enter branch name manually')).toBe(input)
})

test('a branch recreated at another commit previews the commit the lookup found now', async () => {
  // Out of the capped list: found at A, then deleted (404 on refresh, which
  // switches to manual entry), then recreated at B.
  const shaA = 'a'.repeat(40)
  const shaB = 'b'.repeat(40)
  let head: string | null = shaA
  const commit = (sha: string, message: string) => () =>
    jsonResponse({
      exists: true,
      commit_sha: sha,
      commit: {
        sha,
        message,
        author: 'Example Author',
        author_email: 'author@example.com',
        date: '2026-01-01T00:00:00Z',
      },
    })
  const { view } = renderModal({
    truncated: true,
    extraRoutes: {
      [`GET /repository/11/commits/${shaA}`]: commit(shaA, 'The first main'),
      [`GET /repository/11/commits/${shaB}`]: commit(
        shaB,
        'The recreated main'
      ),
    },
    lookup: () =>
      head
        ? jsonResponse({
            name: 'main',
            commit_sha: head,
            protected: false,
            is_default: true,
          })
        : jsonResponse({ title: 'Branch Not Found' }, 404),
  })

  await pickStaging(view)
  await waitFor(() => expect(view.getByText(/The first main/)).toBeTruthy())

  head = null
  fireEvent.click(view.getByTitle('Refresh branches'))
  const input = await view.findByPlaceholderText('Enter branch name manually')
  await waitFor(() => expect(view.getByText(NOT_FOUND)).toBeTruthy())
  // A deleted branch previews no commit at all.
  expect(view.queryByText('Commit to deploy')).toBeNull()

  head = shaB
  fireEvent.click(view.getByRole('button', { name: 'Check again' }))
  await waitFor(() => expect(view.getByText(/The recreated main/)).toBeTruthy())
  expect(view.queryByText(/The first main/)).toBeNull()
  expect(view.getByPlaceholderText('Enter branch name manually')).toBe(input)
})
