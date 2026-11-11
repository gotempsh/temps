// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { AddDomain } from './AddDomain'
import { EmailDomainNew } from './EmailDomainNew'

test.each([AddDomain, EmailDomainNew])(
  'domain creation uses one centered shared wizard',
  (Component) => {
    const client = new QueryClient()
    try {
      const html = renderToStaticMarkup(
        <QueryClientProvider client={client}>
          <MemoryRouter>
            <BreadcrumbProvider>
              <Component />
            </BreadcrumbProvider>
          </MemoryRouter>
        </QueryClientProvider>
      )
      expect(html).toContain('mx-auto max-w-5xl')
      expect(html).toContain('aria-label="Setup progress"')
      expect(html.match(/<h1/g)).toHaveLength(1)
      expect(html).not.toContain('overflow-auto')
      expect(html).toContain('rounded-lg border bg-card')
    } finally {
      client.clear()
    }
  }
)
