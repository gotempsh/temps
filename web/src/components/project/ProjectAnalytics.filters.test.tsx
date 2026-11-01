// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ProjectResponse } from '@/api/client'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from '@/components/project-frame/project-environment-context'
import { installBrowserWindow } from '@/test/dom'

// The globe pulls in three.js, which the test runner cannot load.
mock.module('@/components/analytics/EarthGlobe', () => ({
  EarthGlobe: () => null,
}))
const { AnalyticsFilters } = await import('./ProjectAnalytics')

installBrowserWindow()
afterEach(cleanup)

function filters() {
  return (
    <QueryClientProvider client={new QueryClient()}>
      <AnalyticsFilters
        project={{ id: 17 } as ProjectResponse}
        activeFilter="24hours"
        dateRange={undefined}
        selectedEnvironment={undefined}
        onFilterChange={() => {}}
        onDateRangeChange={() => {}}
        onEnvironmentChange={() => {}}
        onRefresh={() => {}}
        isRefreshing={false}
      />
    </QueryClientProvider>
  )
}

test('inside a project the analytics filters leave the environment to the bar', () => {
  const bar = {} as ProjectEnvironmentContextValue
  const view = render(
    <ProjectEnvironmentContext.Provider value={bar}>
      {filters()}
    </ProjectEnvironmentContext.Provider>
  )
  expect(view.queryByRole('combobox', { name: 'Environment' })).toBeNull()
})

test('on their own the analytics filters keep an environment picker', () => {
  const view = render(filters())
  expect(view.getByRole('combobox', { name: 'Environment' })).toBeTruthy()
})
