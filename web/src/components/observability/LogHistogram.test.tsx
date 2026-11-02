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

test('configured log volume is always shown as a short chart, with nothing to expand', () => {
  const html = render(capability)
  expect(html).toContain('Log volume')
  expect(html).not.toContain('global-log-volume-details')
  expect(html).not.toContain('hidden=""')
  expect(html).toContain('aria-label="Group log volume by"')
  // Kept short so the log lines stay the main thing on the page.
  expect(html).not.toContain('h-[180px]')
  expect(html).toContain('h-20')
})

test('unconfigured log volume shows setup guidance in the same compact card', () => {
  const html = render({
    ...capability,
    configured: false,
    reason: 'Line index is unavailable.',
  })
  expect(html).not.toContain('global-log-volume-details')
  expect(html).toContain('Line index is unavailable.')
  expect(html).toContain('Configure in settings')
  // The compact onboarding, not the 220px block.
  expect(html).not.toContain('h-[220px]')
  expect(html).not.toContain('Group log volume by')
})
