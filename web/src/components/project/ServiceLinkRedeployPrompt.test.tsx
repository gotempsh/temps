// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  ProjectDeployContext,
  type OpenDeployOptions,
} from './project-deploy-context'
import { ServiceLinkRedeployPrompt } from './ServiceLinkRedeployPrompt'

installBrowserWindow()
afterEach(cleanup)

const project = { id: 7, slug: 'synthetic-api', source_type: 'git' } as const
const environment = (id: number, slug: string, deployment: number) => ({
  id,
  slug,
  name: slug,
  is_preview: false,
  current_deployment_id: deployment,
  branch: slug === 'production' ? 'main' : slug,
})

/** Two deployed environments; records every request the prompt sends. */
function serveEnvironments() {
  return mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([
        environment(4, 'staging', 41),
        environment(3, 'production', 31),
      ]),
  })
}

function renderPrompt(wrap: (children: ReactNode) => ReactNode = (c) => c) {
  return render(
    <QueryClientProvider
      client={
        new QueryClient({
          defaultOptions: {
            queries: { retry: false },
            mutations: { retry: false },
          },
        })
      }
    >
      <MemoryRouter>
        {wrap(
          <ServiceLinkRedeployPrompt
            project={project}
            change={{ kind: 'linked', serviceName: 'synthetic-db' }}
            onDismiss={() => {}}
          />
        )}
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('on a project page, Redeploy now opens the deploy dialog on production instead of deploying', async () => {
  const api = serveEnvironments()
  const opened: (OpenDeployOptions | undefined)[] = []
  const view = renderPrompt((children) => (
    <ProjectDeployContext.Provider
      value={{ openDeploy: (options) => opened.push(options) }}
    >
      {children}
    </ProjectDeployContext.Provider>
  ))

  const button = (await view.findByRole('button', {
    name: /Redeploy now/,
  })) as HTMLButtonElement
  await waitFor(() => expect(button.disabled).toBe(false))
  fireEvent.click(button)

  expect(opened).toEqual([{ environmentId: 3 }])
  // Nothing is deployed until the user confirms in the dialog.
  expect(api.calls.filter((call) => call.method !== 'GET')).toEqual([])
  expect(api.calls.map((call) => call.path)).not.toContain(
    '/projects/7/deployments/31'
  )
})

test('outside a project page, Redeploy now still redeploys what is running', async () => {
  const api = serveEnvironments()
  const view = renderPrompt()

  const button = (await view.findByRole('button', {
    name: /Redeploy now/,
  })) as HTMLButtonElement
  await waitFor(() => expect(button.disabled).toBe(false))
  fireEvent.click(button)

  // It looks up each environment's running deployment to redeploy it.
  await waitFor(() =>
    expect(api.calls.map((call) => call.path)).toContain(
      '/projects/7/deployments/31'
    )
  )
})
