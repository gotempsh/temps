// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import type { ReactNode } from 'react'
import type { EnvironmentResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from './project-environment-context'
import { useEnvironmentFilter } from './use-environment-filter'

installBrowserWindow()
afterEach(cleanup)

const staging = {
  id: 5,
  name: 'staging',
  slug: 'staging',
} as EnvironmentResponse
const production = {
  id: 3,
  name: 'production',
  slug: 'production',
} as EnvironmentResponse

function bar(
  selection: { environment?: EnvironmentResponse; all?: boolean },
  selected: string[]
) {
  return {
    environments: [production, staging],
    isPending: false,
    isError: false,
    retry: () => {},
    scope: 'all',
    selection: {
      environment: selection.environment,
      all: selection.all ?? false,
      source: 'url',
    },
    select: (value: string) => selected.push(value),
  } as unknown as ProjectEnvironmentContextValue
}

function Filter() {
  const [environmentId, setEnvironmentId] = useEnvironmentFilter()
  return (
    <>
      <p>Filtering: {environmentId ?? 'all'}</p>
      <button type="button" onClick={() => setEnvironmentId(5)}>
        Staging
      </button>
      <button type="button" onClick={() => setEnvironmentId(undefined)}>
        Every environment
      </button>
    </>
  )
}

function inBar(value: ProjectEnvironmentContextValue, children: ReactNode) {
  return (
    <ProjectEnvironmentContext.Provider value={value}>
      {children}
    </ProjectEnvironmentContext.Provider>
  )
}

test('inside a project the filter is the bar: it reads and sets its selection', () => {
  const selected: string[] = []
  const view = render(
    inBar(bar({ environment: production }, selected), <Filter />)
  )

  expect(view.getByText('Filtering: 3')).toBeTruthy()
  fireEvent.click(view.getByText('Staging'))
  fireEvent.click(view.getByText('Every environment'))
  expect(selected).toEqual(['staging', 'all'])
})

test('"All environments" in the bar filters by nothing', () => {
  const view = render(inBar(bar({ all: true }, []), <Filter />))
  expect(view.getByText('Filtering: all')).toBeTruthy()
})

test('outside a project the page keeps its own filter', () => {
  const view = render(<Filter />)
  expect(view.getByText('Filtering: all')).toBeTruthy()
  fireEvent.click(view.getByText('Staging'))
  expect(view.getByText('Filtering: 5')).toBeTruthy()
})
