// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter, Route, Routes } from 'react-router'
import { ServiceLogs } from './ServiceLogs'

const render = (url: string) =>
  renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter initialEntries={[url]}>
        <Routes>
          <Route path="/storage/:id/logs" element={<ServiceLogs />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )

test('log settings stay folded away behind a visible button', () => {
  const html = render('/storage/7/logs')
  expect(html).toContain('Log settings')
  expect(html).toContain('aria-expanded="false"')
  expect(html).not.toContain('Container log ingestion')
})

test('?settings=log-ingestion opens the ingestion budget, as the notice links', () => {
  const html = render('/storage/7/logs?settings=log-ingestion')
  expect(html).toContain('aria-expanded="true"')
  expect(html).toContain('Container log ingestion')
})
