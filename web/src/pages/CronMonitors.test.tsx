// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes, useLocation } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { client as apiClient } from '@/api/client/client.gen'
import { getEnvironmentsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from '@/components/project-frame/project-environment-context'
import CronMonitors from './CronMonitors'
import { CronJobs, MovedToCheckIns } from './CronJobs'

installBrowserWindow()
const original = apiClient.getConfig()
afterEach(() => {
  cleanup()
  apiClient.setConfig(original)
})

test('cron limits validate before transport; a native API error preserves inputs for retry', async () => {
  const requests: Record<string, unknown>[] = []
  apiClient.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const request =
          input instanceof Request ? input : new Request(input, init)
        if (request.method === 'POST') {
          requests.push(await request.json())
          return Response.json(
            {
              title: 'Service Unavailable',
              status: 503,
              detail: 'Synthetic monitor storage unavailable',
            },
            { status: 503 }
          )
        }
        return Response.json([])
      },
      { preconnect: fetch.preconnect }
    ),
  })
  const query = new QueryClient({
    defaultOptions: {
      queries: { retry: false, staleTime: Infinity },
      mutations: { retry: false },
    },
  })
  query.setQueryData(
    getEnvironmentsOptions({ path: { project_id: 17 } }).queryKey,
    []
  )
  const project = {
    id: 17,
    slug: 'synthetic',
    name: 'Synthetic',
  } as ProjectResponse
  const view = render(
    <QueryClientProvider client={query}>
      <MemoryRouter initialEntries={['/new']}>
        <CronMonitors project={project} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const form = view.getByLabelText('Monitor slug').closest('form')!
  fireEvent.change(view.getByLabelText('Monitor slug'), {
    target: { value: 'synthetic-job' },
  })
  fireEvent.change(view.getByLabelText('Failures before alert'), {
    target: { value: '101' },
  })
  await act(async () => {
    fireEvent.submit(form)
  })
  expect(requests.length).toBe(0)
  expect(view.container.textContent).toContain(
    'Enter a whole number from 1 to 100.'
  )
  fireEvent.change(view.getByLabelText('Failures before alert'), {
    target: { value: '2' },
  })
  await act(async () => {
    fireEvent.submit(form)
  })
  await waitFor(() =>
    expect(view.container.textContent).toContain(
      'Synthetic monitor storage unavailable'
    )
  )
  expect(requests.length).toBe(1)
  expect(requests[0]).toEqual({
    slug: 'synthetic-job',
    environment_id: null,
    config: {
      schedule: { type: 'crontab', value: '0 * * * *' },
      timezone: 'UTC',
      checkin_margin: 1,
      max_runtime: 30,
      failure_issue_threshold: 2,
      recovery_threshold: 1,
    },
  })
  expect((view.getByLabelText('Monitor slug') as HTMLInputElement).value).toBe(
    'synthetic-job'
  )
  await act(async () => {
    fireEvent.submit(form)
  })
  await waitFor(() => expect(requests.length).toBe(2))
  query.clear()
})

/** Serves the cron pages' reads; records every path requested. */
function serveCronList(
  paths: string[],
  { jobs = true, monitors = false } = {}
) {
  apiClient.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const request =
          input instanceof Request ? input : new Request(input, init)
        const path = new URL(request.url).pathname.replace(/^\/api/, '')
        paths.push(path)
        const crons = path.match(/^\/projects\/17\/environments\/(\d+)\/crons$/)
        if (crons && !jobs) return Response.json([])
        if (crons)
          return Response.json([
            {
              id: 9,
              path: `/api/tick-${crons[1]}`,
              schedule: '*/10 * * * *',
              environment_id: Number(crons[1]),
              project_id: 17,
              created_at: '2026-01-01T00:00:00Z',
              next_run: null,
              mode: 'http',
              concurrency_policy: 'allow',
              retry_limit: 0,
            },
          ])
        if (path === '/projects/17/environments')
          return Response.json([
            { id: 3, name: 'production', slug: 'production' },
          ])
        if (path === '/projects/17/cron-monitors')
          return Response.json(
            monitors
              ? {
                  items: [
                    {
                      id: 4,
                      slug: 'nightly-export',
                      status: 'ok',
                      environment_id: null,
                      config: null,
                      last_check_in_at: null,
                      next_expected_at: null,
                    },
                  ],
                  total: 1,
                  page: 1,
                  page_size: 20,
                }
              : { items: [], total: 0, page: 1, page_size: 20 }
          )
        return Response.json({ configured: true })
      },
      { preconnect: fetch.preconnect }
    ),
  })
}

function CurrentLocation() {
  const location = useLocation()
  return (
    <output data-testid="location">
      {location.pathname + location.search}
    </output>
  )
}

/**
 * Renders the project's cron routes, as ProjectDetail registers them for a
 * hosted service, at `url` under a project bar set to `environment`.
 */
function renderCronRoutes(
  url: string,
  environment: { id: number; name: string; slug: string } | undefined,
  barState: { isError?: boolean; retry?: () => void } = {}
) {
  const query = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const project = {
    id: 17,
    slug: 'synthetic',
    name: 'Synthetic',
  } as ProjectResponse
  const bar = {
    environments: environment ? [environment] : [],
    isPending: false,
    isError: false,
    retry: () => {},
    ...barState,
    selection: { environment, all: false, source: 'url' },
  } as unknown as ProjectEnvironmentContextValue
  return render(
    <QueryClientProvider client={query}>
      <MemoryRouter initialEntries={[url]}>
        <ProjectEnvironmentContext.Provider value={bar}>
          <Routes>
            <Route path="/projects/synthetic">
              <Route
                path="cron-jobs"
                element={<CronJobs project={project} />}
              />
              <Route
                path="cron-jobs/*"
                element={<MovedToCheckIns project={project} />}
              />
              <Route
                path="check-ins/*"
                element={<CronMonitors project={project} />}
              />
            </Route>
          </Routes>
          <CurrentLocation />
        </ProjectEnvironmentContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

const production = { id: 3, name: 'production', slug: 'production' }

test('Cron jobs lists the .temps.yaml jobs Temps runs, and nothing about check-ins but a pointer', async () => {
  const paths: string[] = []
  serveCronList(paths)
  const view = renderCronRoutes('/projects/synthetic/cron-jobs', production)

  const job = await view.findByRole('link', { name: /\/api\/tick-3/ })
  expect(job.getAttribute('href')).toBe(
    '/projects/synthetic/settings/cron-jobs/3/9'
  )
  expect(view.getByRole('button', { name: /How to add a job/ })).toBeTruthy()
  expect(view.queryByRole('link', { name: 'Create monitor' })).toBeNull()
  expect(
    view.getByRole('link', { name: 'Set up a check-in' }).getAttribute('href')
  ).toBe('/projects/synthetic/check-ins')
  expect(paths).not.toContain('/projects/17/cron-monitors')
})

test('the jobs follow the environment chosen in the project bar', async () => {
  const paths: string[] = []
  serveCronList(paths)
  const view = renderCronRoutes('/projects/synthetic/cron-jobs', {
    id: 5,
    name: 'staging',
    slug: 'staging',
  })

  await view.findByRole('link', { name: /\/api\/tick-5/ })
  expect(paths).toContain('/projects/17/environments/5/crons')
  expect(paths).not.toContain('/projects/17/environments/3/crons')
  // The bar picks the environment; the page has no picker of its own.
  expect(view.queryByLabelText('Environment')).toBeNull()
})

test('a failed environments read is reported with a retry, not as "no environments"', () => {
  serveCronList([])
  let retries = 0
  const view = renderCronRoutes('/projects/synthetic/cron-jobs', undefined, {
    isError: true,
    retry: () => retries++,
  })

  expect(view.getByText('Could not load environments')).toBeTruthy()
  expect(view.queryByRole('heading', { name: 'No cron jobs yet' })).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'Retry' }))
  expect(retries).toBe(1)
})

test('with no jobs, Cron jobs is the .temps.yaml guide itself', async () => {
  serveCronList([], { jobs: false })
  const view = renderCronRoutes('/projects/synthetic/cron-jobs', production)

  expect(
    await view.findByRole('heading', { name: 'No cron jobs in production' })
  ).toBeTruthy()
  // The snippet is on the page, not behind a button.
  expect(view.getByText(/at your repository root/)).toBeTruthy()
  expect(view.queryByRole('button', { name: /How to add a job/ })).toBeNull()
  expect(view.queryByRole('dialog')).toBeNull()
  expect(view.queryByRole('link', { name: 'Create monitor' })).toBeNull()
})

test('with no monitors, Check-ins offers one Create monitor and the SDK guide', async () => {
  const paths: string[] = []
  serveCronList(paths)
  const view = renderCronRoutes('/projects/synthetic/check-ins', production)

  expect(
    await view.findByRole('heading', { name: 'No check-in monitors yet' })
  ).toBeTruthy()
  expect(
    view
      .getAllByRole('link', { name: 'Create monitor' })
      .map((link) => link.getAttribute('href'))
  ).toEqual(['/projects/synthetic/check-ins/new'])
  expect(view.getByText('Send check-ins from your job')).toBeTruthy()
  expect(view.queryByRole('button', { name: /Refresh/ })).toBeNull()
  // The jobs Temps runs are another page.
  expect(paths.some((path) => path.endsWith('/crons'))).toBe(false)
})

test('Check-ins lists the monitors, with Refresh and Create monitor in the header', async () => {
  serveCronList([], { monitors: true })
  const view = renderCronRoutes('/projects/synthetic/check-ins', production)

  const monitor = await view.findByRole('link', { name: 'nightly-export' })
  expect(monitor.getAttribute('href')).toBe('/projects/synthetic/check-ins/4')
  expect(view.getByRole('button', { name: /Refresh/ })).toBeTruthy()
  expect(view.getAllByRole('link', { name: 'Create monitor' })).toHaveLength(1)
  expect(view.queryByText('Send check-ins from your job')).toBeNull()
})

test('an old monitor link under cron-jobs opens the same page under check-ins', async () => {
  serveCronList([])
  const view = renderCronRoutes(
    '/projects/synthetic/cron-jobs/4/edit?from=alarm',
    production
  )

  await waitFor(() =>
    expect(view.getByTestId('location').textContent).toBe(
      '/projects/synthetic/check-ins/4/edit?from=alarm'
    )
  )
})
