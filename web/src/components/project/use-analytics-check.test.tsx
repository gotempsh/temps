// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { useState, type ReactNode } from 'react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { useAnalyticsCheck } from './use-analytics-check'

installBrowserWindow()
afterEach(cleanup)

const HAS_EVENTS_PATH = '/projects/7/has-events'
const COUNTS_PATH = '/projects/7/unique-counts'

function renderCheck(
  hasEvents: () => Response | Promise<Response>,
  newEvents: number
) {
  const api = mockApi({
    [`GET ${HAS_EVENTS_PATH}`]: hasEvents,
    [`GET ${COUNTS_PATH}`]: () => jsonResponse({ count: newEvents }),
  })
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const { result } = renderHook(
    () => {
      const [waiting, setWaiting] = useState(false)
      const check = useAnalyticsCheck(7, waiting)
      return {
        ...check,
        begin: () => {
          check.start()
          setWaiting(true)
        },
      }
    },
    {
      wrapper: ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={queries}>{children}</QueryClientProvider>
      ),
    }
  )
  const asked = (path: string) => api.calls.some((call) => call.path === path)
  return { result, queries, asked }
}

const answered = (queries: QueryClient) =>
  waitFor(() => expect(queries.isFetching()).toBe(0))

test('a project that already had events does not pass by having them', async () => {
  const { result, queries, asked } = renderCheck(
    () => jsonResponse({ has_events: true }),
    0
  )
  await answered(queries)

  act(() => result.current.begin())
  await waitFor(() => expect(asked(COUNTS_PATH)).toBe(true))
  await answered(queries)

  expect(result.current.hadEventsBefore).toBe(true)
  expect(result.current.passed).toBe(false)
})

test('a project that already had events passes on a new one', async () => {
  const { result, queries } = renderCheck(
    () => jsonResponse({ has_events: true }),
    1
  )
  await answered(queries)

  act(() => result.current.begin())

  await waitFor(() => expect(result.current.passed).toBe(true))
})

test('a project with no events waits for its first without counting new ones', async () => {
  const { result, queries, asked } = renderCheck(
    () => jsonResponse({ has_events: false }),
    1
  )
  await answered(queries)

  act(() => result.current.begin())
  await answered(queries)

  expect(result.current.hadEventsBefore).toBe(false)
  expect(result.current.passed).toBe(false)
  expect(asked(COUNTS_PATH)).toBe(false)
})

test('starting before the first answer asks for a new event, so it cannot claim success', async () => {
  const { result, asked } = renderCheck(() => new Promise(() => {}), 0)

  act(() => result.current.begin())

  await waitFor(() => expect(asked(COUNTS_PATH)).toBe(true))
  expect(result.current.hadEventsBefore).toBe(true)
  expect(result.current.passed).toBe(false)
})
