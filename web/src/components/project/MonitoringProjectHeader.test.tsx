// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from '@/components/project-frame/project-environment-context'
import { installBrowserWindow } from '@/test/dom'
import { MonitoringProjectHeader } from './MonitoringProjectOverview'

installBrowserWindow()
afterEach(cleanup)

const production = {
  id: 3,
  name: 'production',
  slug: 'production',
  is_preview: false,
} as EnvironmentResponse

function renderHeader(
  environments: EnvironmentResponse[],
  state: { isPending?: boolean; isError?: boolean; retry?: () => void } = {}
) {
  const bar = {
    environments,
    isPending: false,
    isError: false,
    retry: () => {},
    ...state,
    scope: 'all',
    selection: { environment: environments[0], all: false, source: 'url' },
    select: () => {},
    defaultEnvironment: environments[0],
  } as unknown as ProjectEnvironmentContextValue
  return render(
    <MemoryRouter initialEntries={['/projects/synthetic/errors']}>
      <ProjectEnvironmentContext.Provider value={bar}>
        <MonitoringProjectHeader
          project={
            { id: 1, slug: 'synthetic', name: 'Synthetic' } as ProjectResponse
          }
        />
      </ProjectEnvironmentContext.Provider>
    </MemoryRouter>
  )
}

test('a monitoring-only project picks the environment its pages filter by', () => {
  const view = renderHeader([production])
  const nav = view.getByRole('navigation', { name: 'Project and environment' })
  expect(nav.textContent).toContain('Synthetic')
  expect(view.getByRole('combobox', { name: 'Environment' })).toBeTruthy()
  // Only the picker: deploying is not something this project does.
  expect(view.queryByRole('button', { name: /Deploy/ })).toBeNull()
})

test('with no environments there is nothing to pick, and it says so', () => {
  const view = renderHeader([])
  expect(view.queryByRole('combobox', { name: 'Environment' })).toBeNull()
  expect(view.getByText('No environments yet')).toBeTruthy()
})

test('while environments load the picker has a placeholder, not a gap', () => {
  const view = renderHeader([], { isPending: true })
  expect(view.queryByText('No environments yet')).toBeNull()
  expect(
    view.container.querySelector('[data-slot="skeleton"], .animate-pulse')
  ).toBeTruthy()
})

test('a failed environments read offers a retry instead of hiding the picker', () => {
  let retries = 0
  const view = renderHeader([], { isError: true, retry: () => retries++ })
  expect(view.queryByText('No environments yet')).toBeNull()
  fireEvent.click(
    view.getByRole('button', { name: 'Environments failed to load · Retry' })
  )
  expect(retries).toBe(1)
})
