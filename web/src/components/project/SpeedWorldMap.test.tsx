// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { SpeedWorldMap } from './SpeedWorldMap'

installBrowserWindow()
afterEach(cleanup)

test('a moved window keeps the map on screen while it loads', async () => {
  let release: (() => void) | undefined
  let reads = 0
  mockApi({
    'GET /performance/page-metrics': async () => {
      if (++reads > 1) await new Promise<void>((r) => (release = r))
      return jsonResponse({ groups: [] })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const map = (start: string, end: string) => (
    <QueryClientProvider client={client}>
      <SpeedWorldMap
        projectId={17}
        environmentId={1}
        startDate={start}
        endDate={end}
        device="desktop"
        includeBots={false}
        filters={{}}
        onCountryClick={() => {}}
      />
    </QueryClientProvider>
  )
  const view = render(map('2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z'))
  await waitFor(() =>
    expect(view.container.querySelector('svg')).not.toBeNull()
  )

  view.rerender(map('2026-01-01T00:05:00Z', '2026-01-02T00:05:00Z'))

  await waitFor(() => expect(release).toBeDefined())
  expect(view.container.querySelector('svg')).not.toBeNull()
  expect(
    view.container.querySelector('[data-slot="skeleton"], .animate-pulse')
  ).toBeNull()
  release?.()
})
