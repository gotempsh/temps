// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client'
import { EnvironmentsTabsView } from './EnvironmentsTabsView'

installBrowserWindow()
afterEach(cleanup)

test('a service with no environments gets an empty state, not a second page title', () => {
  const query = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  query.setQueryData(
    getEnvironmentsOptions({ path: { project_id: 17 } }).queryKey,
    []
  )
  const view = render(
    <QueryClientProvider client={query}>
      <MemoryRouter>
        <EnvironmentsTabsView
          project={{ id: 17, slug: 'synthetic' } as ProjectResponse}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )

  // The section tab already says "Environments".
  expect(view.queryByRole('heading', { name: 'Environments' })).toBeNull()
  expect(view.getByText('No environments')).toBeTruthy()
  expect(view.getByRole('button', { name: 'New environment' })).toBeTruthy()
})
