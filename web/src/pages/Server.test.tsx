// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { Server } from './Server'

test('server renders its own heading and controls without alert settings or preferences queries', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/monitoring/server']}>
        <BreadcrumbProvider>
          <Server />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(markup.match(/<h1/g)).toHaveLength(1)
  expect(markup).toContain('Server</h1>')
  expect(markup).toContain('Pause')
  expect(markup).not.toContain('Monitoring &amp; Alerts')
  expect(markup).not.toContain('role="tablist"')
  expect(
    client.getQueryCache().find({ queryKey: ['preferences'] })
  ).toBeUndefined()
  client.clear()
})
