// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  useBranchVerification,
  type BranchVerificationInput,
} from './useBranchVerification'

installBrowserWindow()
afterEach(cleanup)

/** Renders the hook's verdict as text so the assertions read plainly. */
function Probe(props: BranchVerificationInput) {
  const verification = useBranchVerification(props)
  return (
    <>
      <span>
        {verification.status}:{verification.branch?.commit_sha ?? 'none'}
      </span>
      <button type="button" onClick={() => void verification.refresh()}>
        refresh
      </button>
    </>
  )
}

function renderProbe(
  routes: Parameters<typeof mockApi>[0],
  props: Partial<BranchVerificationInput> = {}
) {
  const api = mockApi(routes)
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <Probe
        repoOwner="temps"
        repoName="example"
        connectionId={7}
        branch="main"
        enabled
        {...props}
      />
    </QueryClientProvider>
  )
  return { api, view }
}

test('a branch the server returns is found, with its commit', async () => {
  const { view } = renderProbe({
    'GET /repositories/temps/example/branch': () =>
      jsonResponse({
        name: 'main',
        commit_sha: 'a'.repeat(40),
        protected: false,
        is_default: true,
      }),
  })

  await waitFor(() =>
    expect(view.getByText(`found:${'a'.repeat(40)}`)).toBeTruthy()
  )
})

test('a 404 is the only answer that means the branch does not exist', async () => {
  const { view } = renderProbe({
    'GET /repositories/temps/example/branch': () =>
      jsonResponse({ title: 'Branch Not Found' }, 404),
  })

  await waitFor(() => expect(view.getByText('missing:none')).toBeTruthy())
})

test('any other failure leaves the question open rather than answering it', async () => {
  for (const status of [401, 403, 429, 502]) {
    const { view } = renderProbe({
      'GET /repositories/temps/example/branch': () =>
        jsonResponse({ title: 'Nope' }, status),
    })

    await waitFor(() => expect(view.getByText('unverified:none')).toBeTruthy())
    cleanup()
  }
})

test('nothing is asked when the caller has nothing to verify', () => {
  const { api, view } = renderProbe({}, { enabled: false })

  expect(view.getByText('not-needed:none')).toBeTruthy()
  expect(api.called('GET', '/repositories/temps/example/branch')).toBe(false)
})

test('a public repository is verified through the public endpoint', async () => {
  const { view } = renderProbe(
    {
      'GET /git/public/github/temps/example/branch': () =>
        jsonResponse({
          name: 'main',
          commit_sha: 'b'.repeat(40),
          protected: false,
          is_default: true,
        }),
    },
    { connectionId: undefined, publicProvider: 'github' }
  )

  await waitFor(() =>
    expect(view.getByText(`found:${'b'.repeat(40)}`)).toBeTruthy()
  )
})

test('refresh asks the provider again, so a failed lookup can be retried', async () => {
  let attempts = 0
  const { view } = renderProbe({
    'GET /repositories/temps/example/branch': () => {
      attempts += 1
      return attempts === 1
        ? jsonResponse({ title: 'Rate limit exceeded' }, 429)
        : jsonResponse({
            name: 'main',
            commit_sha: 'b'.repeat(40),
            protected: false,
            is_default: false,
          })
    },
  })

  await waitFor(() => expect(view.getByText('unverified:none')).toBeTruthy())
  fireEvent.click(view.getByText('refresh'))
  await waitFor(() =>
    expect(view.getByText(`found:${'b'.repeat(40)}`)).toBeTruthy()
  )
  expect(attempts).toBe(2)
})

test('refresh spends no provider request when nothing is being verified', async () => {
  const { api, view } = renderProbe(
    {
      'GET /repositories/temps/example/branch': () =>
        jsonResponse({ title: 'Branch Not Found' }, 404),
    },
    { enabled: false }
  )

  fireEvent.click(view.getByText('refresh'))
  await waitFor(() => expect(view.getByText('not-needed:none')).toBeTruthy())
  expect(api.called('GET', '/repositories/temps/example/branch')).toBe(false)
})

test('a refresh that answers 404 reports the branch missing, not the cached find', async () => {
  let deleted = false
  const { view } = renderProbe({
    'GET /repositories/temps/example/branch': () =>
      deleted
        ? jsonResponse({ title: 'Branch Not Found' }, 404)
        : jsonResponse({
            name: 'main',
            commit_sha: 'a'.repeat(40),
            protected: false,
            is_default: false,
          }),
  })

  await waitFor(() =>
    expect(view.getByText(`found:${'a'.repeat(40)}`)).toBeTruthy()
  )
  deleted = true
  fireEvent.click(view.getByText('refresh'))
  await waitFor(() => expect(view.getByText('missing:none')).toBeTruthy())
})

test('a refresh that fails for another reason leaves the branch unverified', async () => {
  let limited = false
  const { view } = renderProbe({
    'GET /repositories/temps/example/branch': () =>
      limited
        ? jsonResponse({ title: 'Rate limit exceeded' }, 429)
        : jsonResponse({
            name: 'main',
            commit_sha: 'a'.repeat(40),
            protected: false,
            is_default: false,
          }),
  })

  await waitFor(() =>
    expect(view.getByText(`found:${'a'.repeat(40)}`)).toBeTruthy()
  )
  limited = true
  fireEvent.click(view.getByText('refresh'))
  await waitFor(() => expect(view.getByText('unverified:none')).toBeTruthy())
})

test('the branch is looked up by its exact name, non-breaking space included', async () => {
  const asked: (string | null)[] = []
  const { view } = renderProbe(
    {
      'GET /repositories/temps/example/branch': (request) => {
        asked.push(new URL(request.url).searchParams.get('name'))
        return jsonResponse({ title: 'Branch Not Found' }, 404)
      },
    },
    { branch: 'release ' }
  )

  await waitFor(() => expect(view.getByText('missing:none')).toBeTruthy())
  expect(asked).toEqual(['release '])
})
