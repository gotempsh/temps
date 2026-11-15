// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { DeploymentResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
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
