// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import {
  getSandboxQueryKey,
  listSandboxesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SandboxPreviewPasswordCard } from './SandboxPreviewPasswordCard'

installBrowserWindow()
afterEach(cleanup)

// The share-link card reads `preview_password_hint` from the sandbox detail,
// so setting a password has to refresh that exact query, not an ad-hoc key.
test('setting a password invalidates the sandbox detail and list queries', async () => {
  const api = mockApi({
    'PUT /v1/sandboxes/sbx_1/preview-password': () =>
      jsonResponse({ preview_password_hint: '1234' }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const detailKey = getSandboxQueryKey({ path: { id: 'sbx_1' } })
  const listKey = listSandboxesQueryKey({ query: { page: 1, page_size: 20 } })
  client.setQueryData(detailKey, { sandbox: {} })
  client.setQueryData(listKey, { sandboxes: [] })

  const view = render(
    <QueryClientProvider client={client}>
      <SandboxPreviewPasswordCard sandboxId="sbx_1" hint={undefined} />
    </QueryClientProvider>
  )

  const inputs = view.container.querySelectorAll('input')
  expect(inputs.length).toBeGreaterThanOrEqual(2)
  for (const input of Array.from(inputs).slice(0, 2)) {
    fireEvent.change(input, { target: { value: 'a-long-enough-password' } })
  }
  fireEvent.submit(inputs[0]!.closest('form')!)

  await view.findByText(/Preview password|ends in/).catch(() => null)
  await new Promise((resolve) => setTimeout(resolve, 100))

  expect(api.called('PUT', '/v1/sandboxes/sbx_1/preview-password')).toBe(true)
  expect(client.getQueryState(detailKey)?.isInvalidated).toBe(true)
  expect(client.getQueryState(listKey)?.isInvalidated).toBe(true)
  api.restore()
})
