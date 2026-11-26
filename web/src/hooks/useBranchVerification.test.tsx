// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
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
    <span>
      {verification.status}:{verification.branch?.commit_sha ?? 'none'}
    </span>
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
