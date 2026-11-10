// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { collectRouteQueryFailures } from '@/lib/route-query-failures'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { useScreenshotsEnabled } from './useScreenshotsEnabled'

installBrowserWindow()
afterEach(cleanup)

function setup() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  )
  return { client, wrapper }
}

test('a role refused the platform settings sees screenshots off, with no page failure', async () => {
  const api = mockApi({
    'GET /settings': () =>
      jsonResponse({ title: 'Access denied', detail: 'settings:read' }, 403),
  })
  const { client, wrapper } = setup()
  const { result } = renderHook(() => useScreenshotsEnabled(), { wrapper })

  await waitFor(() =>
    expect(client.getQueryCache().findAll()[0]?.state.status).toBe('error')
  )
  expect(result.current).toBe(false)
  expect(collectRouteQueryFailures(client)).toEqual([])
  api.restore()
})

test('reflects the platform setting when it can be read', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ screenshots: { enabled: true } }),
  })
  const { wrapper } = setup()
  const { result } = renderHook(() => useScreenshotsEnabled(), { wrapper })

  await waitFor(() => expect(result.current).toBe(true))
  api.restore()
})
