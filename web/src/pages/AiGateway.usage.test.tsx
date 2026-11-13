// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { UsageAnalytics } from './AiGateway'

installBrowserWindow()
const originalFetch = globalThis.fetch
afterEach(() => {
  cleanup()
  globalThis.fetch = originalFetch
})

function json(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: { 'Content-Type': 'application/json' },
  })
}

/**
 * Usage endpoints whose summary answers are released by the test, so it can
 * look at the cards while a request is in flight.
 */
function mockUsage() {
  const pending: Array<{ hours: number; release: (requests: number) => void }> =
    []
  globalThis.fetch = (async (input: Request | string | URL) => {
    const url = new URL(String(input), 'http://localhost')
    if (url.pathname === '/api/ai/usage/summary') {
      const hours =
        (Date.parse(url.searchParams.get('to')!) -
          Date.parse(url.searchParams.get('from')!)) /
        3_600_000
      return new Promise<Response>((resolve) =>
        pending.push({
          hours,
          release: (requests) =>
            resolve(
              json({
                total_requests: requests,
                total_tokens: 0,
                total_input_tokens: 0,
                total_output_tokens: 0,
                avg_latency_ms: 0,
                error_count: 0,
              })
            ),
        })
      )
    }
    if (url.pathname === '/api/ai/usage/recent')
      return json({ entries: [], total: 0 })
    if (url.pathname.startsWith('/api/ai/usage/')) return json([])
    return new Response('{}', { status: 404 })
  }) as typeof fetch
  return {
    async answer(requests: number) {
      await screen.findByText('Requests')
      const request = await waitForRequest(pending)
      request.release(requests)
      return request
    },
    pending,
  }
}

async function waitForRequest<T>(pending: T[]): Promise<T> {
  for (let i = 0; i < 100 && pending.length === 0; i++)
    await new Promise((resolve) => setTimeout(resolve, 10))
  const next = pending.shift()
  if (!next) throw new Error('no usage summary request was sent')
  return next
}

function renderUsage() {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <UsageAnalytics />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('picking another range shows loading, not the previous range numbers', async () => {
  const usage = mockUsage()
  renderUsage()
  expect((await usage.answer(1_200)).hours).toBe(24)
  await screen.findByText('1.2K')

  fireEvent.click(screen.getByRole('button', { name: '7d' }))

  const week = await waitForRequest(usage.pending)
  expect(week.hours).toBe(7 * 24)
  expect(screen.queryByText('1.2K')).toBeNull()
  week.release(9_900)
  await screen.findByText('9.9K')
})

test('Refresh keeps the numbers on screen while the moved window loads', async () => {
  const usage = mockUsage()
  renderUsage()
  await usage.answer(1_200)
  await screen.findByText('1.2K')
  await new Promise((resolve) => setTimeout(resolve, 5))

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  const moved = await waitForRequest(usage.pending)
  expect(moved.hours).toBe(24)
  expect(screen.getByText('1.2K')).toBeTruthy()
  moved.release(1_300)
  await screen.findByText('1.3K')
})

test('a failed usage read says so instead of showing zero usage', async () => {
  globalThis.fetch = (async (input: Request | string | URL) => {
    const url = new URL(String(input), 'http://localhost')
    if (url.pathname === '/api/ai/usage/recent')
      return json({ entries: [], total: 0 })
    if (url.pathname.startsWith('/api/ai/usage/'))
      return new Response('{}', { status: 500 })
    return new Response('{}', { status: 404 })
  }) as typeof fetch
  renderUsage()

  await screen.findByText("Couldn't load requests over time.")
  expect(screen.getAllByText("Couldn't load")).toHaveLength(5)
  expect(screen.queryByText('No usage data')).toBeNull()
  expect(screen.queryByText('$0.00')).toBeNull()
  expect(screen.getAllByRole('button', { name: 'Retry' }).length).toBe(5)
})

test('Retry on a failed breakdown shows loading, never "No data", while it runs', async () => {
  let failing = true
  let releaseProviders: (() => void) | undefined
  globalThis.fetch = (async (input: Request | string | URL) => {
    const url = new URL(String(input), 'http://localhost')
    if (url.pathname === '/api/ai/usage/recent')
      return json({ entries: [], total: 0 })
    if (url.pathname === '/api/ai/usage/by-provider') {
      if (failing) return new Response('{}', { status: 500 })
      return new Promise<Response>((resolve) => {
        releaseProviders = () => resolve(json([]))
      })
    }
    if (url.pathname.startsWith('/api/ai/usage/')) return json([])
    return new Response('{}', { status: 404 })
  }) as typeof fetch
  renderUsage()
  // The card's content: the failure, then the loading state, then the answer.
  const card = (await screen.findByText("Couldn't load usage by provider."))
    .parentElement!.parentElement!

  failing = false
  fireEvent.click(within(card).getByRole('button', { name: 'Retry' }))

  await waitFor(() => expect(releaseProviders).toBeDefined())
  expect(within(card).queryByText('No data')).toBeNull()
  expect(card.querySelector('.animate-pulse')).not.toBeNull()
  await act(async () => releaseProviders!())
  await within(card).findByText('No data')
})
