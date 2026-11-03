// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectDrop } from './ProjectDrop'

installBrowserWindow()

let api: ReturnType<typeof mockApi> | undefined
afterEach(() => {
  cleanup()
  api?.restore()
  api = undefined
})

const project = {
  id: 7,
  slug: 'example',
  name: 'Example',
  directory: '.',
  preset: 'nextjs',
} as ProjectResponse

const environment = (id: number, name: string) => ({
  id,
  name,
  slug: name,
  is_preview: false,
})

function serve() {
  api = mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([
        environment(3, 'production'),
        environment(4, 'staging'),
        environment(5, 'qa'),
      ]),
    'POST /drop/inspect': () =>
      jsonResponse({
        suggestedName: 'example',
        candidates: [
          {
            directory: '.',
            preset: 'nextjs',
            label: 'Next.js',
            isStatic: false,
            confidence: 'high',
            reason: 'Found a next.config file',
          },
        ],
      }),
    'POST /projects/7/environments/3/deploy/source': () =>
      jsonResponse({ id: 31 }),
    'POST /projects/7/environments/4/deploy/source': () =>
      jsonResponse({ id: 41 }),
  })
  return api
}

function Location() {
  const { pathname, search } = useLocation()
  return <div data-testid="location">{`${pathname}${search}`}</div>
}

function renderDrop(entry: string) {
  const view = render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={[entry]}>
        <Location />
        <Routes>
          <Route
            path="/projects/example/drop"
            element={<ProjectDrop project={project} />}
          />
          <Route path="*" element={null} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
  const selectArchive = () =>
    fireEvent.change(view.getByLabelText('Project archive or HTML'), {
      target: {
        files: [new File(['zip'], 'app.zip', { type: 'application/zip' })],
      },
    })
  const location = () => view.getByTestId('location').textContent
  return { view, selectArchive, location }
}

test('starting one of several redeploys stays here on the next environment', async () => {
  const calls = serve()
  const { view, selectArchive, location } = renderDrop(
    '/projects/example/drop?environment=3&pending=3&pending=4&pending=5'
  )

  await view.findByText('Redeploying 3 environments')
  view.getByText(/Still to redeploy after that: staging, qa/)

  selectArchive()
  const deploy = await view.findByRole('button', { name: 'Deploy Next.js' })
  fireEvent.click(deploy)

  await view.findByText('Deployment to production started')
  expect(calls.called('POST', '/projects/7/environments/3/deploy/source')).toBe(
    true
  )
  await waitFor(() =>
    expect(location()).toBe(
      '/projects/example/drop?environment=4&pending=4&pending=5'
    )
  )
  view.getByText(/Still to redeploy after that: qa/)
  // The way to what just started stays one click away.
  expect(
    view.getByRole('link', { name: 'View deployment' }).getAttribute('href')
  ).toBe('/projects/example/deployments/31')

  // The same files go to the next environment, without choosing them again.
  fireEvent.click(view.getByRole('button', { name: 'Deploy Next.js' }))
  await waitFor(() =>
    expect(
      calls.called('POST', '/projects/7/environments/4/deploy/source')
    ).toBe(true)
  )
  await view.findByText('Deployment to staging started')
  await waitFor(() =>
    expect(location()).toBe('/projects/example/drop?environment=5&pending=5')
  )
})

test('the last environment waiting goes on to its deployment', async () => {
  serve()
  const { view, selectArchive, location } = renderDrop(
    '/projects/example/drop?environment=3&pending=3'
  )

  selectArchive()
  fireEvent.click(await view.findByRole('button', { name: 'Deploy Next.js' }))

  await waitFor(() =>
    expect(location()).toBe('/projects/example/deployments/31')
  )
})

test('an environment skipped for another is still waited for', async () => {
  serve()
  const { view, selectArchive, location } = renderDrop(
    '/projects/example/drop?environment=4&pending=3&pending=4'
  )

  selectArchive()
  fireEvent.click(await view.findByRole('button', { name: 'Deploy Next.js' }))

  await view.findByText('Deployment to staging started')
  await waitFor(() =>
    expect(location()).toBe('/projects/example/drop?environment=3&pending=3')
  )
})

test('an upload on its own says nothing about other environments', async () => {
  serve()
  const { view, selectArchive, location } = renderDrop(
    '/projects/example/drop?environment=3'
  )

  await view.findByLabelText('Project archive or HTML')
  selectArchive()
  await view.findByRole('button', { name: 'Deploy Next.js' })
  expect(view.queryByText(/Redeploying/)).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'Deploy Next.js' }))
  await waitFor(() =>
    expect(location()).toBe('/projects/example/deployments/31')
  )
})
