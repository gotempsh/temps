// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  QueryClient,
  QueryClientProvider,
  QueryObserver,
} from '@tanstack/react-query'
import { act, cleanup, render, screen } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { markReadFailureRetained } from '@/lib/route-query-failures'
import { RouteQueryFailureBanner } from './RouteQueryFailureBanner'

installBrowserWindow()
afterEach(cleanup)

async function failedRead(client: QueryClient, error: unknown) {
  const observer = new QueryObserver(client, {
    queryKey: ['visits', { start_date: 1, end_date: 2 }],
    queryFn: () => Promise.reject(error),
    retry: false,
  })
  const stop = observer.subscribe(() => {})
  await observer.refetch().catch(() => {})
  return stop
}

test('a moved window that did not load says the latest data is unavailable, over the rows kept', async () => {
  const client = new QueryClient()
  const error = { status: 500, title: 'Internal Server Error' }
  const stop = await failedRead(client, error)
  render(
    <QueryClientProvider client={client}>
      <RouteQueryFailureBanner />
    </QueryClientProvider>
  )
  await screen.findByText('Data on this page unavailable')

  act(() => markReadFailureRetained(error))

  await screen.findByText('Latest data on this page unavailable')
  expect(screen.getByRole('alert').textContent).toContain(
    'Showing last-known data'
  )
  expect(screen.getByRole('button', { name: 'Retry' })).toBeTruthy()
  stop()
})
