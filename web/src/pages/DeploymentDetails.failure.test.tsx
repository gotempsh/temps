// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { DeploymentResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { CancelledReason } from './DeploymentDetails'

installBrowserWindow()
afterEach(cleanup)

test.each(['container_exited', 'out_of_memory'] as const)(
  'opens %s evidence when a running deployment fails, and preserves a manual collapse',
  (code) => {
    const client = new QueryClient({
      defaultOptions: { queries: { staleTime: Infinity } },
    })
    client.setQueryData(['lastSuccessfulDeployment', 1, 2, 3], null)
    const running = {
      id: 3,
      project_id: 1,
      environment_id: 2,
      status: 'running',
      cancelled_reason: null,
      failure: null,
    } as DeploymentResponse
    const failed = {
      ...running,
      status: 'failed',
      cancelled_reason:
        'Container exited during startup (exit code 1).\nContainer logs for unhealthy/stopped services:\nmissing required setting',
      failure: {
        code,
        stage: 'runtime',
        title: 'Container exited',
        remediation: 'Check the startup logs.',
        classifier_version: 7,
      },
    } as DeploymentResponse
    const view = (deployment: DeploymentResponse) => (
      <QueryClientProvider client={client}>
        <MemoryRouter>
          <CancelledReason deployment={deployment} projectSlug="app" />
        </MemoryRouter>
      </QueryClientProvider>
    )
    const screen = render(view(running))
    screen.rerender(view(failed))
    expect(screen.getByText(/missing required setting/)).toBeTruthy()
    const collapse = screen.getByRole('button', { name: 'Collapse error' })
    expect(collapse.getAttribute('aria-expanded')).toBe('true')
    fireEvent.click(collapse)
    expect(screen.queryByText(/missing required setting/)).toBeNull()
    screen.rerender(view({ ...failed }))
    expect(
      screen
        .getByRole('button', { name: 'Show full error' })
        .getAttribute('aria-expanded')
    ).toBe('false')
  }
)

test("a collapse on one deployment does not hide the next deployment's startup error", () => {
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity } },
  })
  const failed = (id: number) =>
    ({
      id,
      project_id: 1,
      environment_id: 2,
      status: 'failed',
      cancelled_reason: `Container exited during startup (exit code 1).\nContainer logs for unhealthy/stopped services:\ncrash in deployment ${id}`,
      failure: {
        code: 'container_exited',
        stage: 'runtime',
        title: 'Container exited',
        remediation: 'Check the startup logs.',
        classifier_version: 7,
      },
    }) as DeploymentResponse
  for (const id of [3, 4]) {
    client.setQueryData(['lastSuccessfulDeployment', 1, 2, id], null)
  }
  const view = (deployment: DeploymentResponse) => (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <CancelledReason deployment={deployment} projectSlug="app" />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const screen = render(view(failed(3)))
  fireEvent.click(screen.getByRole('button', { name: 'Collapse error' }))
  expect(screen.queryByText(/crash in deployment 3/)).toBeNull()

  screen.rerender(view(failed(4)))
  expect(screen.getByText(/crash in deployment 4/)).toBeTruthy()
  expect(
    screen
      .getByRole('button', { name: 'Collapse error' })
      .getAttribute('aria-expanded')
  ).toBe('true')
})

test('a deployment whose entrypoint needs a capability offers the one-click fix in the banner', async () => {
  mockApi({
    'GET /projects/1/container-security': () =>
      jsonResponse({
        allowed_capabilities: [],
        options: [],
        always_enforced: [],
        can_edit: true,
        edit_requirement: 'An instance administrator can change it.',
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false } },
  })
  client.setQueryData(['lastSuccessfulDeployment', 1, 2, 5], null)
  const deployment = {
    id: 5,
    project_id: 1,
    environment_id: 2,
    status: 'failed',
    cancelled_reason:
      'Container keeps restarting during startup [entrypoint_needs_capability: NET_BIND_SERVICE verified] (restarted 5 times; last exit: exit code 1)',
    failure: {
      code: 'entrypoint_needs_capability',
      stage: 'runtime',
      title: 'Entrypoint needs a Linux capability',
      remediation: 'Allow it for this project or change the image.',
      settings_section: 'container_security',
      classifier_version: 9,
      suggested_fix: {
        required_capabilities: ['NET_BIND_SERVICE'],
        verified: true,
        allow_capabilities: ['NET_BIND_SERVICE'],
        not_allowed_capabilities: [],
        setting: 'allowed_capabilities',
        setting_path: '/projects/1/container-security',
        image_fix: 'Remove the file capabilities from the image.',
      },
    },
  } as unknown as DeploymentResponse
  const screen = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <CancelledReason
          deployment={deployment}
          projectSlug="app"
          onRedeploySameSource={async () => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  expect(screen.getByText(/Entrypoint needs a Linux capability/)).toBeTruthy()
  expect(
    await screen.findByRole('button', {
      name: 'Allow NET_BIND_SERVICE for this project and redeploy',
    })
  ).toBeTruthy()
})

test('the capability fix confirmation does not carry over to another deployment', async () => {
  mockApi({
    'GET /projects/1/container-security': () =>
      jsonResponse({
        allowed_capabilities: [],
        options: [],
        always_enforced: [],
        can_edit: true,
        edit_requirement: 'An instance administrator can change it.',
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { staleTime: Infinity, retry: false } },
  })
  const failedWithFix = (id: number) =>
    ({
      id,
      project_id: 1,
      environment_id: 2,
      status: 'failed',
      cancelled_reason:
        'Container keeps restarting during startup [entrypoint_needs_capability: NET_BIND_SERVICE verified] (restarted 5 times)',
      failure: {
        code: 'entrypoint_needs_capability',
        stage: 'runtime',
        title: 'Entrypoint needs a Linux capability',
        remediation: 'Allow it for this project or change the image.',
        settings_section: 'container_security',
        classifier_version: 9,
        suggested_fix: {
          required_capabilities: ['NET_BIND_SERVICE'],
          verified: true,
          allow_capabilities: ['NET_BIND_SERVICE'],
          not_allowed_capabilities: [],
          setting: 'allowed_capabilities',
          setting_path: '/projects/1/container-security',
          image_fix: 'Remove the file capabilities from the image.',
        },
      },
    }) as unknown as DeploymentResponse
  client.setQueryData(['lastSuccessfulDeployment', 1, 2, 5], null)
  client.setQueryData(['lastSuccessfulDeployment', 1, 2, 6], null)
  const view = (deployment: DeploymentResponse) => (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <CancelledReason
          deployment={deployment}
          projectSlug="app"
          onRedeploySameSource={async () => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const name = 'Allow NET_BIND_SERVICE for this project and redeploy'
  const screen = render(view(failedWithFix(5)))
  fireEvent.click(await screen.findByRole('button', { name }))
  fireEvent.click(await screen.findByRole('checkbox'))
  expect(screen.getByRole('alertdialog')).toBeTruthy()

  screen.rerender(view(failedWithFix(6)))
  expect(screen.queryByRole('alertdialog')).toBeNull()
  expect(await screen.findByRole('button', { name })).toBeTruthy()
})
