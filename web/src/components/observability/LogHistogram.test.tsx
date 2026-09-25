// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { AnalyticsCapability } from '@/api/client/types.gen'
import { LogHistogram } from './LogHistogram'

const capability: AnalyticsCapability = {
  configured: true,
  example: 'Group lines by service.',
  setup_path: '/settings/metrics-monitoring',
  live_chunks: 2,
  indexed_chunks: 2,
  forget_backlog: 0,
}

function render(config: AnalyticsCapability) {
  return renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <LogHistogram
          filters={{
            start_time: '2026-09-18T00:00:00Z',
            end_time: '2026-09-18T12:00:00Z',
          }}
          capability={config}
          capabilityLoading={false}
          onRangeSelect={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('configured log volume starts as a compact summary above the log list', () => {
  const html = render(capability)
  expect(html).toContain('aria-expanded="false"')
  expect(html).toContain('id="global-log-volume-details" hidden=""')
  expect(html).toContain('Log volume')
})

test('unconfigured log volume shows setup guidance immediately', () => {
  const html = render({
    ...capability,
    configured: false,
    reason: 'Line index is unavailable.',
  })
  expect(html).toContain('aria-expanded="true"')
  expect(html).toContain('Line index is unavailable.')
  expect(html).toContain('Configure in Settings')
  expect(html).not.toContain('id="global-log-volume-details" hidden=""')
})
