// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { VisitorAnalyticsCard } from './ProjectOverview'

installBrowserWindow()
afterEach(cleanup)

function renderCard(
  overrides: Partial<Parameters<typeof VisitorAnalyticsCard>[0]>
) {
  return render(
    <MemoryRouter>
      <VisitorAnalyticsCard
        projectSlug="example"
        capabilityPending={false}
        capabilityError={false}
        enabled
        visitors={0}
        pageViews={0}
        returningVisitors={0}
        returningRate={0}
        hourlyVisitors={[]}
        loading={false}
        error={false}
        {...overrides}
      />
    </MemoryRouter>
  )
}

test('analytics that never received an event offers to set it up', () => {
  const view = renderCard({ enabled: false })

  expect(view.getByText('Add browser analytics')).toBeTruthy()
  expect(
    view.getByRole('link', { name: 'Set up analytics' }).getAttribute('href')
  ).toBe('/projects/example/analytics/setup')
  expect(view.queryByRole('link', { name: /Explore/ })).toBeNull()
})

test('a quiet day says so instead of a flat chart and four zeros', () => {
  const view = renderCard({})

  expect(view.getByText('No visitors in the last 24 hours')).toBeTruthy()
  expect(view.queryByText('Page views')).toBeNull()
  expect(view.queryByText('Returning rate')).toBeNull()
  expect(
    view.getByRole('link', { name: 'Check setup' }).getAttribute('href')
  ).toBe('/projects/example/analytics/setup')
  // Older visits are still there to explore.
  expect(view.getByRole('link', { name: /Explore/ })).toBeTruthy()
})

test('any traffic shows the numbers', () => {
  const view = renderCard({ visitors: 3, pageViews: 9 })

  expect(view.getByText('Page views')).toBeTruthy()
  expect(view.getByText('9')).toBeTruthy()
  expect(view.queryByText('No visitors in the last 24 hours')).toBeNull()
})
