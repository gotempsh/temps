// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, useLocation } from 'react-router'
import { PageHeader, PageTitle } from '@temps-sdk/ds'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import {
  rememberedEnvironmentKey,
  useProjectEnvironment,
} from './project-environment-context'
import { ProjectFrame, ProjectSectionChrome } from './ProjectFrame'

installBrowserWindow()
afterEach(() => {
  cleanup()
  window.localStorage.clear()
})

const environments = ['production', 'staging', 'preview'].map(
  (slug, index) => ({ id: index + 1, name: slug, slug }) as EnvironmentResponse
)

function Selected() {
  const { selection } = useProjectEnvironment()
  return <p>Selected: {selection.environment?.slug}</p>
}

test('going straight to another project reads that project’s remembered environment', () => {
  window.localStorage.setItem(rememberedEnvironmentKey(1), 'staging')
  window.localStorage.setItem(rememberedEnvironmentKey(2), 'preview')
  const query = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  for (const id of [1, 2])
    query.setQueryData(
      getEnvironmentsOptions({ path: { project_id: id } }).queryKey,
      environments
    )
  const frame = (project: { id: number; slug: string }) => (
    <QueryClientProvider client={query}>
      <MemoryRouter initialEntries={['/projects/first/environments']}>
        <ProjectFrame project={project as ProjectResponse}>
          <Selected />
        </ProjectFrame>
      </MemoryRouter>
    </QueryClientProvider>
  )

  const view = render(frame({ id: 1, slug: 'first' }))
  expect(view.getByText('Selected: staging')).toBeTruthy()
  // Same mounted route element, cached project: only the project changes.
  view.rerender(frame({ id: 2, slug: 'second' }))
  expect(view.getByText('Selected: preview')).toBeTruthy()
})

test('a link that still names the environment by id opens that environment', () => {
  const query = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  query.setQueryData(
    getEnvironmentsOptions({ path: { project_id: 1 } }).queryKey,
    environments
  )
  const view = render(
    <QueryClientProvider client={query}>
      <MemoryRouter
        initialEntries={['/projects/first/environments?environment=2']}
      >
        <ProjectFrame project={{ id: 1, slug: 'first' } as ProjectResponse}>
          <Selected />
        </ProjectFrame>
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(view.getByText('Selected: staging')).toBeTruthy()
})

function Search() {
  return <output data-testid="search">{useLocation().search}</output>
}

test('a legacy environment link is rewritten as ?env=, so the next tab keeps it', async () => {
  const query = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  query.setQueryData(
    getEnvironmentsOptions({ path: { project_id: 1 } }).queryKey,
    environments
  )
  const project = { id: 1, slug: 'first' } as ProjectResponse
  const view = render(
    <QueryClientProvider client={query}>
      <MemoryRouter initialEntries={['/projects/first/errors?environment=2']}>
        <ProjectFrame project={project}>
          <ProjectSectionChrome project={project}>
            <Selected />
          </ProjectSectionChrome>
          <Search />
        </ProjectFrame>
      </MemoryRouter>
    </QueryClientProvider>
  )

  await waitFor(() =>
    expect(view.getByTestId('search').textContent).toBe('?env=staging')
  )
  expect(view.getByText('Selected: staging')).toBeTruthy()
  const alerts = view.getByRole('link', { name: 'Alerts' })
  expect(alerts.getAttribute('href')).toBe(
    '/projects/first/errors/alert-rules?env=staging'
  )
})

test('page titles inside a project are one level below the project name', () => {
  const query = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  query.setQueryData(
    getEnvironmentsOptions({ path: { project_id: 1 } }).queryKey,
    environments
  )
  const view = render(
    <QueryClientProvider client={query}>
      <MemoryRouter initialEntries={['/projects/first/storage']}>
        <ProjectFrame project={{ id: 1, slug: 'first' } as ProjectResponse}>
          <PageHeader title="Databases" description="Synthetic page." />
          <PageTitle>KV store</PageTitle>
        </ProjectFrame>
      </MemoryRouter>
    </QueryClientProvider>
  )

  expect(
    view.getByRole('heading', { level: 2, name: 'Databases' })
  ).toBeTruthy()
  expect(view.getByRole('heading', { level: 2, name: 'KV store' })).toBeTruthy()
  expect(view.queryByRole('heading', { level: 1 })).toBeNull()
})
