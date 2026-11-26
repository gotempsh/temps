// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
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

/** Production deploys `main`, which is the branch under test. */
const environments = [
  {
    id: 1,
    name: 'production',
    slug: 'production',
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
  lookup: () => Response
  /** Omit to leave the lookup unmocked, i.e. still in flight / 404-by-default. */
  withLookup?: boolean
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
