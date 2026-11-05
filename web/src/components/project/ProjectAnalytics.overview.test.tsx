// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, mock, test } from 'bun:test'
import { cleanup, fireEvent, render, within } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import {
  AiAssistantContext,
  type AiAssistantValue,
} from '@/components/ai/AiAssistantContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'

// The globe pulls in three.js, which the test runner cannot load.
mock.module('@/components/analytics/EarthGlobe', () => ({
  EarthGlobe: () => null,
}))
const { ProjectAnalyticsOverview } = await import('./ProjectAnalytics')

installBrowserWindow()
beforeEach(() => {
  localStorage.clear()
  mockApi({})
})
afterEach(cleanup)

function header() {
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <AiAssistantContext.Provider value={{} as unknown as AiAssistantValue}>
        <MemoryRouter>
          <ProjectAnalyticsOverview
            project={{ id: 17, slug: 'demo', name: 'Demo' } as ProjectResponse}
          />
        </MemoryRouter>
      </AiAssistantContext.Provider>
    </QueryClientProvider>
  )
  const element = view.container.querySelector('[data-page-header]')
  if (!element) throw new Error('The overview rendered no page header')
  return { view, row: within(element as HTMLElement) }
}

test('the overview header offers only the time filter', () => {
  const { row } = header()
  expect(
    row.getByText('Traffic, audience, and engagement for Demo.')
  ).toBeTruthy()
  expect(row.getByRole('button', { name: '24h' })).toBeTruthy()
  // The globe has its own tab.
  expect(row.queryByText('Visitor globe')).toBeNull()
  expect(row.queryByRole('button', { name: 'Refresh' })).toBeNull()
  expect(row.queryByRole('button', { name: 'Show insights' })).toBeNull()
})

test('insights stay reachable from the overview', () => {
  const { view } = header()
  fireEvent.click(view.getByRole('button', { name: 'Show insights' }))
  expect(view.getByRole('button', { name: 'Hide insights' })).toBeTruthy()
})
