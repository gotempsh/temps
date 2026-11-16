// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { nodeCapabilityGetQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { FirstProjectOnboarding } from './FirstProjectOnboarding'

test('empty installs lead with the sample app before credentials or migration', () => {
  const client = new QueryClient()
  client.setQueryData(nodeCapabilityGetQueryKey(), { schedulable: true })
  const markup = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <FirstProjectOnboarding />
      </MemoryRouter>
    </QueryClientProvider>
  )
  client.clear()
  expect(markup).toContain('Deploy sample app')
  expect(markup.indexOf('Deploy sample app')).toBeLessThan(
    markup.indexOf('Already hosting your application elsewhere?')
  )
  expect(markup).toContain('/projects/new?source=templates')
})
