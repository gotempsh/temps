// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { SessionDiagnostics } from './SessionDiagnostics'

test('debug mode exposes session diagnostics without fetching until opened', () => {
  const client = new QueryClient()
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/ai-first?debug=true']}>
        <SessionDiagnostics publicId="example-conversation" />
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(html).toContain('Session JSON')
  expect(client.isFetching()).toBe(0)
  expect(
    client.getQueryState(['conversation-diagnostics', 'example-conversation'])
      ?.fetchStatus
  ).toBe('idle')
  client.clear()
})

test('normal chat never renders session JSON or creates its diagnostics query', () => {
  for (const path of ['/ai-first', '/ai-first?debug=false', '/ai-first?debug=1']) {
    const client = new QueryClient()
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[path]}>
          <SessionDiagnostics publicId="example-conversation" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(html).not.toContain('Session JSON')
    expect(client.getQueryCache().getAll()).toHaveLength(0)
    client.clear()
  }
})
