// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { EmailDomainsManagement } from './EmailDomainsManagement'

function render(client: QueryClient) {
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <EmailDomainsManagement />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('email domains without a provider links directly to provider setup', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false } },
  })
  client.setQueryData(['email-providers'], [])
  client.setQueryData(['email-domains'], [])
  const html = render(client)
  expect(html).toContain('No email providers configured')
  expect(html).toContain('href="/email/providers/new"')
  expect(html).toContain('Add provider')
  expect(html).not.toContain('href="/email/domains/new"')
  client.clear()
})

for (const failedKey of ['email-providers', 'email-domains']) {
  test(`${failedKey} lookup failure does not claim configuration is missing`, () => {
    const client = new QueryClient({
      defaultOptions: {
        queries: { staleTime: Infinity, retry: false, retryOnMount: false },
      },
    })
    client.setQueryData(
      [failedKey === 'email-providers' ? 'email-domains' : 'email-providers'],
      []
    )
    client
      .getQueryCache()
      .build(client, { queryKey: [failedKey] })
      .setState({
        status: 'error',
        error: new TypeError('Failed to fetch'),
        fetchStatus: 'idle',
      })
    const html = render(client)
    expect(html).toContain('Email data unavailable')
    expect(html).toContain('Retry email data')
    expect(html).not.toContain('No email providers configured')
    expect(html).not.toContain('No email domains configured')
    client.clear()
  })
}
