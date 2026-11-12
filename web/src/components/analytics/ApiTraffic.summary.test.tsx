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
        total_requests: 120,
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
      prompts.push(JSON.parse(String(init?.body)).prompt)
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
  return { timeseriesEnds, summaryRouteEnds, prompts }
}

function renderTab(open: AiAssistantValue['open'] = () => {}) {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AiAssistantContext.Provider
        value={{ open } as unknown as AiAssistantValue}
      >
        <MemoryRouter initialEntries={['/api-traffic?filter=24hours']}>
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
  expect(prompts[0]).not.toContain(moved)
})
