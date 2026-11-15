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

test('costs show what was recorded, not a recomputation from base list prices', async () => {
  // A long-context request: billed at the higher rate, so recomputing it from
  // the base $0.10 / $0.50 per 1M would show about a fifth of the spend.
  const entry = {
    id: 1,
    timestamp: '2026-11-15T10:00:00Z',
    provider: 'anthropic',
    model: 'claude-haiku-5-5',
    input_tokens: 2_000_000,
    output_tokens: 0,
    latency_ms: 12,
    estimated_cost_microcents: 1_250_000,
    status: 200,
    is_streaming: false,
    is_byok: false,
  }
  globalThis.fetch = (async (input: Request | string | URL) => {
    const url = new URL(String(input), 'http://localhost')
    if (url.pathname === '/api/ai/pricing')
      return json({
        models: [
          {
            model: 'claude-haiku-5-5',
            provider: 'anthropic',
            input_per_million: 0.1,
            output_per_million: 0.5,
          },
        ],
      })
    if (url.pathname === '/api/ai/usage/summary')
      return json({
        total_requests: 1,
        total_tokens: 2_000_000,
        total_input_tokens: 2_000_000,
        total_output_tokens: 0,
        avg_latency_ms: 12,
        total_cost_microcents: 3_500_000,
        error_count: 0,
        streaming_count: 0,
        byok_count: 0,
      })
    if (url.pathname === '/api/ai/usage/top-models')
      return json([
        {
          model: 'claude-haiku-5-5',
          provider: 'anthropic',
          request_count: 1,
          input_tokens: 2_000_000,
          output_tokens: 0,
          total_tokens: 2_000_000,
          avg_latency_ms: 12,
          estimated_cost_microcents: 2_250_000,
        },
      ])
    if (url.pathname === '/api/ai/usage/recent')
      return json({ entries: [entry], total: 1 })
    if (url.pathname.startsWith('/api/ai/usage/')) return json([])
    return new Response('{}', { status: 404 })
  }) as typeof fetch
  renderUsage()

  // Est. Cost card: the summary's recorded total, across every model.
  await screen.findByText('$3.50')
  // Top models: the model's recorded cost.
  await screen.findByText('$2.25')
  // Recent requests: the request's recorded cost.
  await screen.findByText('$1.25')
  // Never the base-rate figure: 2,000,000 x $0.10 per 1M.
  expect(screen.queryByText('$0.200')).toBeNull()
})

function mockSummary(summary: Record<string, number>) {
  globalThis.fetch = (async (input: Request | string | URL) => {
    const url = new URL(String(input), 'http://localhost')
    if (url.pathname === '/api/ai/usage/summary')
      return json({
        total_tokens: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        avg_latency_ms: 0,
        error_count: 0,
        streaming_count: 0,
        byok_count: 0,
        ...summary,
      })
    if (url.pathname === '/api/ai/usage/recent')
      return json({ entries: [], total: 0 })
    if (url.pathname.startsWith('/api/ai/usage/')) return json([])
    return new Response('{}', { status: 404 })
  }) as typeof fetch
}

test('a window whose requests have no recorded cost is not shown as free', async () => {
  mockSummary({
    total_requests: 3,
    total_cost_microcents: 0,
    unpriced_requests: 3,
  })
  renderUsage()

  await screen.findByText('No recorded cost')
  expect(screen.queryByText('$0.00')).toBeNull()
})

test('a total that leaves out unpriced requests says how many', async () => {
  mockSummary({
    total_requests: 5,
    total_cost_microcents: 1_500_000,
    unpriced_requests: 2,
  })
  renderUsage()

  await screen.findByText('$1.50')
  await screen.findByText('Excludes 2 unpriced requests')
})
