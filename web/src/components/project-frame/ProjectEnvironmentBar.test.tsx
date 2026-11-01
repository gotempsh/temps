// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type {
  EnvironmentResponse,
  ProjectGroupResponse,
  ProjectResponse,
} from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectEnvironmentBar } from './ProjectEnvironmentBar'
import {
  ProjectEnvironmentContext,
  type ProjectEnvironmentContextValue,
} from './project-environment-context'

installBrowserWindow()
afterEach(cleanup)

const production = {
  id: 3,
  name: 'production',
  slug: 'production',
  is_preview: false,
} as EnvironmentResponse

const service = {
  id: 7,
  slug: 'synthetic-api',
  name: 'synthetic-api',
  hosting: 'hosted',
  source_type: 'manual',
} as ProjectResponse

const group = {
  id: 2,
  name: 'Synthetic shop',
  slug: 'synthetic-shop',
  projects: [{ id: 7, name: 'synthetic-api', slug: 'synthetic-api' }],
} as unknown as ProjectGroupResponse

const liveProduction = {
  ...production,
  current_deployment_id: 40,
  sleeping: false,
  main_url: 'https://synthetic-api.example.test',
} as EnvironmentResponse

type Routes = Parameters<typeof mockApi>[0]

function renderBar(
  project: ProjectResponse,
  membership: ProjectGroupResponse | null = null,
  {
    environment = production,
    routes = {},
  }: { environment?: EnvironmentResponse; routes?: Routes } = {}
) {
  mockApi({
    'GET /projects/7/group': () => jsonResponse({ group: membership }),
    ...routes,
  })
  const bar = {
    environments: [environment],
    isPending: false,
    isError: false,
    retry: () => {},
    scope: 'single',
    selection: { environment, all: false, source: 'url' },
    select: () => {},
    defaultEnvironment: environment,
    setDefault: () => {},
  } as unknown as ProjectEnvironmentContextValue
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/projects/synthetic-api/project']}>
        <ProjectEnvironmentContext.Provider value={bar}>
          <ProjectEnvironmentBar project={project} onDeploy={() => {}} />
        </ProjectEnvironmentContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** Opens the bar's "⋯" menu and returns it. */
async function openActions(view: ReturnType<typeof render>) {
  fireEvent.pointerDown(view.getByRole('button', { name: 'Project actions' }), {
    button: 0,
    ctrlKey: false,
    pointerType: 'mouse',
  })
  return view.findByRole('menu')
}

test('the project name is the page heading, and a standalone service can join a project from the menu', async () => {
  const view = renderBar(service)

  expect(
    view.getByRole('heading', { level: 1, name: 'synthetic-api' })
  ).toBeTruthy()
  expect(view.getByRole('button', { name: 'Deploy' })).toBeTruthy()
  expect(view.queryByText('External', { exact: true })).toBeNull()
  // Grouping is one click away, not a control in the row.
  expect(view.queryByRole('button', { name: 'Add to project' })).toBeNull()
  const menu = await openActions(view)
  expect(
    await within(menu).findByRole('menuitem', { name: 'Add to project…' })
  ).toBeTruthy()
})

test('a grouped service names its project and switches between its services', async () => {
  const view = renderBar(service, group)

  expect(
    await view.findByRole('link', { name: 'Project Synthetic shop' })
  ).toBeTruthy()
  expect(
    view.getByRole('button', {
      name: 'synthetic-api: switch to another service in Synthetic shop',
    })
  ).toBeTruthy()
  const menu = await openActions(view)
  expect(
    within(menu).queryByRole('menuitem', { name: 'Add to project…' })
  ).toBeNull()
})

test('an app hosted elsewhere is marked External and offers no deploy', () => {
  const view = renderBar({ ...service, hosting: 'external' } as ProjectResponse)

  expect(view.getByText('External', { exact: true })).toBeTruthy()
  expect(view.queryByRole('button', { name: 'Deploy' })).toBeNull()
})

test('host Docker access is stated wherever the project is open', () => {
  const view = renderBar({
    ...service,
    docker_socket: { granted: true, nodes: [] },
  } as unknown as ProjectResponse)

  expect(
    view.getByRole('img', {
      name: 'Host Docker access: Granted on this installation.',
    })
  ).toBeTruthy()
})

test('without the grant there is no host-access badge', () => {
  const view = renderBar({
    ...service,
    docker_socket: { granted: false, nodes: [] },
  } as unknown as ProjectResponse)

  expect(view.queryByRole('img', { name: /Host Docker access/ })).toBeNull()
})

const deployments = (status: string) => () =>
  jsonResponse({ deployments: [{ id: 91, status }], total: 1 })

test('a failed latest deployment reads Failed and opens that deployment', async () => {
  const view = renderBar(service, null, {
    environment: liveProduction,
    routes: { 'GET /projects/7/deployments': deployments('failed') },
  })

  const failed = await view.findByRole('link', { name: /^Failed: / })
  expect(failed.getAttribute('href')).toBe(
    '/projects/synthetic-api/deployments/91'
  )
  expect(failed.getAttribute('aria-label')).toContain(
    'previous version is still serving'
  )
})

test('monitors that cannot reach production read Down and open Uptime', async () => {
  const view = renderBar(service, null, {
    environment: liveProduction,
    routes: {
      'GET /projects/7/deployments': deployments('completed'),
      'GET /monitors-health/projects': () =>
        jsonResponse({ projects: { '7': { project_id: 7, status: 'down' } } }),
    },
  })

  const down = await view.findByRole('link', { name: /^Down: / })
  expect(down.getAttribute('href')).toBe('/projects/synthetic-api/monitors')
})

test('a healthy environment reads Live, without a link', async () => {
  const view = renderBar(service, null, {
    environment: liveProduction,
    routes: { 'GET /projects/7/deployments': deployments('completed') },
  })

  expect(await view.findByRole('img', { name: /^Live: / })).toBeTruthy()
  expect(view.queryByRole('link', { name: /^Live/ })).toBeNull()
})

for (const [workload, containerPort, visible] of [
  ['a worker, which serves nothing to visit,', null, false],
  ['a web service', 3000, true],
] as const)
  test(`${workload} ${visible ? 'offers' : 'offers no'} Visit`, async () => {
    const view = renderBar(
      { ...service, project_type: 'server' } as ProjectResponse,
      null,
      {
        environment: liveProduction,
        routes: {
          'GET /projects/7/deployments': deployments('completed'),
          'GET /projects/7/environments/3/containers': () =>
            jsonResponse({
              containers: [
                { container_port: containerPort, health_state: 'healthy' },
              ],
            }),
        },
      }
    )

    // Until the containers load, nothing says this is a worker, so the
    // worker's link only goes once they have.
    await waitFor(() =>
      expect(
        view.queryByRole('link', { name: 'Visit production' }) !== null
      ).toBe(visible)
    )
  })

test('the repository opens from the menu, without credentials in the link', async () => {
  const view = renderBar({
    ...service,
    git_url: 'https://token:secret@github.com/example/synthetic-api.git',
  } as ProjectResponse)

  const menu = await openActions(view)
  expect(
    within(menu)
      .getByRole('menuitem', { name: 'Open repository' })
      .getAttribute('href')
  ).toBe('https://github.com/example/synthetic-api')
})

function reachabilityRoute(body: Record<string, unknown>) {
  return {
    'GET /projects/7/environments/3/reachability': () =>
      jsonResponse({ environment_id: 3, addresses: [], ...body }),
  }
}

test('a reachable environment reads Live, and its address is in the status', async () => {
  const view = renderBar(service, null, {
    environment: liveProduction,
    routes: reachabilityRoute({
      state: 'reachable',
      summary: 'Public on the internet.',
      primary_address: { address: 'synthetic-api.example.test' },
      addresses: [
        {
          address: 'synthetic-api.example.test',
          state: 'reachable',
          audience: 'public',
          source: 'environment_domain',
        },
      ],
    }),
  })

  expect(
    await view.findByRole('img', {
      name: 'Live: Public at synthetic-api.example.test.',
    })
  ).toBeTruthy()
})

test('a live environment nobody can reach reads Not reachable and opens the fix', async () => {
  const view = renderBar(service, null, {
    environment: liveProduction,
    routes: reachabilityRoute({
      state: 'not_reachable',
      summary: 'Deployed on the control plane, but nobody can reach it yet.',
    }),
  })

  const status = await view.findByRole('link', { name: /^Not reachable: / })
  expect(status.getAttribute('href')).toBe(
    '/projects/synthetic-api/environments/3/reachability'
  )
  expect(status.getAttribute('aria-label')).toContain('nobody can reach it')
})

test('an app hosted elsewhere reports no deployment state of its own', async () => {
  const view = renderBar(
    { ...service, hosting: 'external' } as ProjectResponse,
    null,
    {
      environment: liveProduction,
      routes: { 'GET /projects/7/deployments': deployments('failed') },
    }
  )

  await view.findByText('External', { exact: true })
  expect(view.queryByRole('link', { name: /^Failed/ })).toBeNull()
  expect(view.queryByRole('img', { name: /^Live/ })).toBeNull()
})

test('the menu holds everything but the two main actions, and on a phone those too', async () => {
  const view = renderBar(
    {
      ...service,
      git_url: 'https://github.com/example/synthetic-api.git',
    } as ProjectResponse,
    null,
    { environment: liveProduction }
  )

  const menu = await openActions(view)
  await within(menu).findByRole('menuitem', { name: 'Add to project…' })
  // Deploy and Visit are menu items only below `sm`; the classes hide
  // them on wider screens, where the bar shows them as buttons.
  for (const name of [
    'Deploy',
    'Visit production',
    'Open repository',
    'Add to project…',
  ])
    expect(within(menu).getByRole('menuitem', { name })).toBeTruthy()
  expect(within(menu).getAllByRole('menuitem')).toHaveLength(4)
})
