// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { GetStarted } from './GetStarted'

function markup(entry: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <BreadcrumbProvider>
          <GetStarted />
        </BreadcrumbProvider>
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  return html
}

test('Get started opens the feature chooser with one page heading', () => {
  const html = markup('/get-started')
  const dom = new DOMParser().parseFromString(html, 'text/html')
  expect(dom.querySelectorAll('h1').length).toBe(1)
  expect(dom.querySelector('h1')?.textContent).toBe(
    'What would you like to try first?'
  )
  expect(html).toContain('/projects/new?source=templates')
  expect(html).toContain('/sandboxes?new=sandbox')
  expect(html).not.toContain('Deploy sample app')
})

test('existing first-deployment tracking links still render their tracker', () => {
  const html = markup('/get-started?project=example-app')
  expect(html).toContain('Deploy your first app')
  expect(html).not.toContain('Choose a starting point')
  expect(html).toContain('Explore the other features')
})
