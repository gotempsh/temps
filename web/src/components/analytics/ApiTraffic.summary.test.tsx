// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from '@/components/ai/AiAssistantContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'

// The globe pulls in three.js, which the test runner cannot load.
mock.module('@/components/analytics/EarthGlobe', () => ({
  EarthGlobe: () => null,
}))
const { ApiTrafficTab } = await import('./ApiTraffic')

installBrowserWindow()
const originalFetch = globalThis.fetch
afterEach(() => {
  cleanup()
  globalThis.fetch = originalFetch
})

const base = '/projects/17/api-analytics'

function mockTraffic() {
  const timeseriesEnds: string[] = []
  const summaryRouteEnds: string[] = []
  const prompts: string[] = []
  const refreshes: boolean[] = []
  const traffic = { total: 120, routesFail: false }
  mockApi({
    [`GET ${base}/proxy-log-access`]: () =>
      jsonResponse({ can_read_proxy_logs: false }),
    [`GET ${base}/timeseries`]: (request) => {
      timeseriesEnds.push(new URL(request.url).searchParams.get('end_date')!)
      return jsonResponse({
        bucket_interval: '1 hour',
        overall_error_rate: 0,
        points: [],
        total_errors: 0,
        total_requests: traffic.total,
      })
    },
    [`POST ${base}/query`]: () =>
      jsonResponse({
        dimensions: [],
        metrics: [],
        page: 1,
        page_size: 20,
        rows: [],
        synthetic_excluded: true,
        total_groups: 0,
        total_pages: 0,
      }),
    [`GET ${base}/routes`]: (request) => {
      summaryRouteEnds.push(new URL(request.url).searchParams.get('end_date')!)
      if (traffic.routesFail)
        return jsonResponse(
          {
            title: 'Internal Server Error',
            status: 500,
            detail: 'Route stats are being rebuilt',
          },
          500
        )
      return jsonResponse({ routes: [], total_routes: 0 })
    },
    [`GET ${base}/callers`]: () =>
      jsonResponse({ callers: [], total_callers: 0 }),
  })
  // The structured summary streams from a plain fetch, not the SDK client.
  const sdkFetch = globalThis.fetch
  globalThis.fetch = (async (input: Request | string | URL, init) => {
    const url = String(input instanceof Request ? input.url : input)
    if (url.endsWith('/projects/17/ai/structured-output/stream')) {
      const body = JSON.parse(String(init?.body))
      prompts.push(body.prompt)
      refreshes.push(body.refresh)
      const value = {
        headline: 'Traffic is steady',
        findings: [],
        anomalies: [],
        recommendation: null,
      }
      return new Response(
        `data: ${JSON.stringify({ type: 'complete', value })}\n\n`,
        { headers: { 'Content-Type': 'text/event-stream' } }
      )
    }
    return sdkFetch(input, init)
  }) as typeof fetch
  return { timeseriesEnds, summaryRouteEnds, prompts, refreshes, traffic }
}

function renderTab(
  open: AiAssistantValue['open'] = () => {},
  entry = '/api-traffic?filter=24hours'
) {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AiAssistantContext.Provider
        value={{ open } as unknown as AiAssistantValue}
      >
        <MemoryRouter initialEntries={[entry]}>
          <ApiTrafficTab
            project={
              {
                id: 17,
                slug: 'demo',
                name: 'demo',
                ai_api_traffic_summary_enabled: true,
              } as ProjectResponse
            }
          />
        </MemoryRouter>
      </AiAssistantContext.Provider>
    </QueryClientProvider>
  )
}

test('Refresh keeps a generated summary on screen and asks the AI nothing new', async () => {
  const traffic = mockTraffic()
  renderTab()
  const generate = await screen.findByRole('button', {
    name: 'Generate AI traffic summary',
  })
  await waitFor(() => expect(generate.hasAttribute('disabled')).toBe(false))

  fireEvent.click(generate)
  await screen.findByText('Traffic is steady')
  expect(traffic.prompts).toHaveLength(1)
  // The summary reused the chart's timeseries instead of fetching it again.
  expect(traffic.timeseriesEnds).toHaveLength(1)
  await new Promise((resolve) => setTimeout(resolve, 5))

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))

  await screen.findByText('Updated just now')
  await waitFor(() => expect(traffic.timeseriesEnds).toHaveLength(2))
  const [first, moved] = traffic.timeseriesEnds
  expect(Date.parse(moved)).toBeGreaterThan(Date.parse(first))
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(screen.getByText('Traffic is steady')).toBeTruthy()
  expect(traffic.prompts).toHaveLength(1)
  expect(traffic.summaryRouteEnds).toEqual([first])
})

test('Investigate with AI after a Refresh inspects the window the summary was made for', async () => {
  const traffic = mockTraffic()
  const prompts: string[] = []
  renderTab(((request: { context: { startPrompt?: string } }) => {
    prompts.push(request.context.startPrompt ?? '')
  }) as unknown as AiAssistantValue['open'])
  const generate = await screen.findByRole('button', {
    name: 'Generate AI traffic summary',
  })
  await waitFor(() => expect(generate.hasAttribute('disabled')).toBe(false))
  fireEvent.click(generate)
  await screen.findByText('Traffic is steady')
  await new Promise((resolve) => setTimeout(resolve, 5))

  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
  await waitFor(() => expect(traffic.timeseriesEnds).toHaveLength(2))
  fireEvent.click(screen.getByRole('button', { name: 'Investigate with AI' }))

  const [summarized, moved] = traffic.timeseriesEnds
  expect(prompts).toHaveLength(1)
  expect(prompts[0]).toContain(`to ${summarized}`)
  expect(prompts[0]).toContain(`The summary below covers `)
  expect(prompts[0]).not.toContain(moved)
})

test('after a Refresh, Generate keeps the open summary; Refresh summary makes a new one for now', async () => {
  const traffic = mockTraffic()
  renderTab()
  const generate = await screen.findByRole('button', {
    name: 'Generate AI traffic summary',
  })
  await waitFor(() => expect(generate.hasAttribute('disabled')).toBe(false))
  fireEvent.click(generate)
  await screen.findByText('Traffic is steady')
  await new Promise((resolve) => setTimeout(resolve, 5))
  fireEvent.click(screen.getByRole('button', { name: 'Refresh' }))
  await waitFor(() => expect(traffic.timeseriesEnds).toHaveLength(2))

  // A cached entry for a rolling preset does not name its window, so asking
  // again here could return the earlier window's summary as the current one.
  fireEvent.click(generate)
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(traffic.prompts).toHaveLength(1)

  fireEvent.click(screen.getByRole('button', { name: 'Refresh summary' }))
  await waitFor(() => expect(traffic.prompts).toHaveLength(2))
  expect(traffic.refreshes).toEqual([false, true])
  expect(traffic.prompts[1]).toContain(traffic.timeseriesEnds[1])
})

test('Refresh summary on a custom range summarizes the traffic it has now', async () => {
  const traffic = mockTraffic()
  renderTab(
    () => {},
    '/api-traffic?filter=custom&from=2026-01-01T00:00:00.000Z&to=2026-01-02T00:00:00.000Z'
  )
  const generate = await screen.findByRole('button', {
    name: 'Generate AI traffic summary',
  })
  await waitFor(() => expect(generate.hasAttribute('disabled')).toBe(false))
  fireEvent.click(generate)
  await screen.findByText('Traffic is steady')
  expect(traffic.prompts[0]).toContain('"requests":120')

  // Late events land in the same fixed window.
  traffic.traffic.total = 480
  fireEvent.click(screen.getByRole('button', { name: 'Refresh summary' }))

  await waitFor(() => expect(traffic.prompts).toHaveLength(2))
  expect(traffic.prompts[1]).toContain('"requests":480')
  expect(traffic.refreshes).toEqual([false, true])
})

test('a summary whose traffic read fails says so, and Try again reads it again', async () => {
  const traffic = mockTraffic()
  renderTab()
  const generate = await screen.findByRole('button', {
    name: 'Generate AI traffic summary',
  })
  await waitFor(() => expect(generate.hasAttribute('disabled')).toBe(false))
  fireEvent.click(generate)
  await screen.findByText('Traffic is steady')

  traffic.traffic.routesFail = true
  fireEvent.click(screen.getByRole('button', { name: 'Refresh summary' }))

  await screen.findByText('Summary unavailable')
  expect(
    screen.getByText(
      "Couldn't read the API traffic to summarize: Route stats are being rebuilt"
    )
  ).toBeTruthy()
  expect(traffic.prompts).toHaveLength(1)

  traffic.traffic.routesFail = false
  fireEvent.click(screen.getByRole('button', { name: 'Try again' }))

  await waitFor(() => expect(traffic.prompts).toHaveLength(2))
  await screen.findByText('Traffic is steady')
})
