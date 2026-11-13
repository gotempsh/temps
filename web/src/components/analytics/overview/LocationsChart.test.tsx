// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { LocationsChart } from './LocationsChart'

installBrowserWindow()
afterEach(cleanup)

test('drilling into a country never lists the countries under it while regions load', async () => {
  let releaseRegions: () => void = () => {}
  mockApi({
    'GET /projects/7/events/properties/breakdown': async (request) => {
      const groupBy = new URL(request.url).searchParams.get('group_by')
      if (groupBy === 'region') {
        await new Promise<void>((resolve) => (releaseRegions = resolve))
        return jsonResponse({ items: [{ value: 'Brittany', count: 4 }] })
      }
      return jsonResponse({
        items: [
          { value: 'France', count: 6 },
          { value: 'Germany', count: 3 },
        ],
      })
    },
  })
  render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <LocationsChart
          project={{ id: 7, slug: 'demo' } as ProjectResponse}
          startDate={new Date('2026-01-01T00:00:00Z')}
          endDate={new Date('2026-01-02T00:00:00Z')}
          environment={undefined}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  fireEvent.click(await screen.findByText('France'))

  // The breadcrumb says France; the country rows must not stay clickable.
  await screen.findByText('All countries')
  expect(screen.queryByText('Germany')).toBeNull()

  releaseRegions()
  await screen.findByText('Brittany')
})
