// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import type { ProjectResponse } from '@/api/client'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from '@/components/ai/AiAssistantContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { OverviewInsights } from './OverviewInsights'
import { moveWindows } from '@/lib/refresh'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 17, slug: 'demo', name: 'demo' } as ProjectResponse
const dayStart = new Date('2026-01-01T00:00:00Z')
const dayEnd = new Date('2026-01-02T00:00:00Z')
const moved = (date: Date) => new Date(date.getTime() + 5 * 60_000)

test('Ask AI waits while a moved window still shows the previous stats', async () => {
  let releaseSessions: (() => void) | undefined
  mockApi({
    'GET /projects/17/unique-counts': async (request) => {
      const query = new URL(request.url).searchParams
      const isMoved = query.get('end_date') !== dayEnd.toISOString()
      if (isMoved && query.get('metric') === 'sessions')
        await new Promise<void>((resolve) => (releaseSessions = resolve))
      return jsonResponse({
        count: query.get('metric') === 'visitors' ? 40 : 60,
      })
    },
    'GET /projects/17/hourly-visits': () => jsonResponse([]),
    'GET /projects/17/events/properties/breakdown': () =>
      jsonResponse({ items: [], property: 'channel', total: 0 }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const view = (start: Date, end: Date) => (
    <QueryClientProvider client={client}>
      <AiAssistantContext.Provider
        value={{ open: () => {} } as unknown as AiAssistantValue}
      >
        <OverviewInsights
          project={project}
          startDate={start}
          endDate={end}
          environment={undefined}
        />
      </AiAssistantContext.Provider>
    </QueryClientProvider>
  )
  const { rerender } = render(view(dayStart, dayEnd))
  const askAi = () => screen.getByRole('button', { name: 'Ask AI' })
  await waitFor(() => expect(askAi().hasAttribute('disabled')).toBe(false))

  // Refresh moves the window.
  moveWindows(() => rerender(view(moved(dayStart), moved(dayEnd))))

  await waitFor(() => expect(releaseSessions).toBeDefined())
  expect(askAi().hasAttribute('disabled')).toBe(true)
  releaseSessions?.()
  await waitFor(() => expect(askAi().hasAttribute('disabled')).toBe(false))
})
