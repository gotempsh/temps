// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, waitFor } from '@testing-library/react'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { useComposeSecurityPolicy } from './useComposeSecurityPolicy'

installBrowserWindow()
afterEach(cleanup)

function Probe({ isCompose }: { isCompose: boolean }) {
  const query = useComposeSecurityPolicy(42, isCompose)
  return <p>{query.isSuccess ? 'loaded' : 'idle'}</p>
}

function renderProbe(isCompose: boolean) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <Probe isCompose={isCompose} />
    </QueryClientProvider>
  )
}

test('a project that is not a Compose project never requests compose-security', async () => {
  const api = mockApi({
    'GET /projects/42/compose-security': () =>
      jsonResponse(
        {
          title: 'Bad Request',
          detail: 'Project 42 does not use Docker Compose',
        },
        400
      ),
  })
  const view = renderProbe(false)
  await new Promise((resolve) => setTimeout(resolve, 100))
  expect(api.calls).toHaveLength(0)
  expect(view.getByText('idle')).toBeTruthy()
  api.restore()
})

test('a Compose project loads its security policy', async () => {
  const api = mockApi({
    'GET /projects/42/compose-security': () => jsonResponse({ policy: {} }),
  })
  const view = renderProbe(true)
  await waitFor(() => expect(view.getByText('loaded')).toBeTruthy())
  expect(api.called('GET', '/projects/42/compose-security')).toBe(true)
  api.restore()
})
