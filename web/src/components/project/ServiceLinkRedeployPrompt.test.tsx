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

/** What each environment's live deployment was made from. */
type Made = 'git' | 'image' | 'archive'
const deploymentOf = (id: number, made: Made) => ({
  id,
  ...(made === 'git' ? { branch: 'main', commit_hash: 'abc123' } : {}),
  ...(made === 'image' ? { metadata: { externalImageRef: 'app:1' } } : {}),
  // A deployment made from an uploaded archive says so itself, whatever the
  // project's own source type is.
  ...(made === 'archive'
    ? { metadata: { deploymentSourceType: 'uploaded_source' } }
    : {}),
})

/** Two deployed environments; records every request the prompt sends. */
function serveEnvironments(made: Made = 'git') {
  return mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([
        environment(4, 'staging', 41),
        environment(3, 'production', 31),
      ]),
    'GET /projects/7/deployments/41': () =>
      jsonResponse(deploymentOf(41, made)),
    'GET /projects/7/deployments/31': () =>
      jsonResponse(deploymentOf(31, made)),
  })
}

/** Resolves once the named button exists and is no longer held back. */
async function enabledButton(
  view: ReturnType<typeof renderPrompt>,
  name: RegExp
) {
  const button = (await view.findByRole('button', {
    name,
  })) as HTMLButtonElement
  await waitFor(() => expect(button.disabled).toBe(false))
  return button
}

function renderPrompt(
  wrap: (children: ReactNode) => ReactNode = (c) => c,
  sourceType: 'git' | 'uploaded_source' | 'manual' = project.source_type
) {
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
            project={{ ...project, source_type: sourceType }}
            change={{ kind: 'linked', serviceName: 'synthetic-db' }}
            onDismiss={() => {}}
          />
        )}
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('on a project page, each environment gets its own deploy dialog, in turn', async () => {
  const api = serveEnvironments()
  const opened: OpenDeployOptions[] = []
  const view = renderPrompt((children) => (
    <ProjectDeployContext.Provider
      value={{ openDeploy: (options) => opened.push(options ?? {}) }}
    >
      {children}
    </ProjectDeployContext.Provider>
  ))

  fireEvent.click(await enabledButton(view, /Redeploy production/))
  expect(opened[0].environmentId).toBe(3)
  // Nothing is deployed until the user confirms in the dialog.
  expect(api.calls.filter((call) => call.method !== 'GET')).toEqual([])

  // Confirming stays here, because staging still needs its redeploy.
  act(() => opened[0].onStarted?.(3))
  expect(view.queryByRole('button', { name: /production/ })).toBeNull()
  expect(view.getByText(/Redeploy staging to apply/)).toBeTruthy()

  // The last one is an ordinary deploy, which goes on to the deployments list.
  fireEvent.click(view.getByRole('button', { name: /Redeploy now/ }))
  expect(opened[1].environmentId).toBe(4)
  expect(opened[1].onStarted).toBeUndefined()
})

test('a Git project whose live deployments are uploaded archives is sent to the upload page, not the Git dialog', async () => {
  serveEnvironments('archive')
  const opened: OpenDeployOptions[] = []
  const view = renderPrompt(
    (children) => (
      <ProjectDeployContext.Provider
        value={{ openDeploy: (options) => opened.push(options ?? {}) }}
      >
        {children}
      </ProjectDeployContext.Provider>
    ),
    'git'
  )

  // An uploaded archive cannot be rebuilt, so the action says it is an upload.
  const staging = await view.findByRole('link', {
    name: /Upload source again for staging/,
  })
  const production = view.getByRole('link', {
    name: /Upload source again for production/,
  })
  expect(view.queryByRole('button', { name: /Redeploy/ })).toBeNull()
  // Straight to the upload page, carrying the environment and every
  // environment still waiting, so the upload can carry on with them.
  expect(staging.getAttribute('href')).toBe(
    '/projects/synthetic-api/drop?environment=4&pending=4&pending=3'
  )
  expect(production.getAttribute('href')).toBe(
    '/projects/synthetic-api/drop?environment=3&pending=4&pending=3'
  )
  // The Git deploy dialog is never opened for it.
  fireEvent.click(staging)
  expect(opened).toEqual([])
})

test('an uploaded-source project gets the same upload link', async () => {
  serveEnvironments('archive')
  const view = renderPrompt(undefined, 'uploaded_source')

  const link = await view.findByRole('link', {
    name: /Upload source again for staging/,
  })
  expect(link.getAttribute('href')).toBe(
    '/projects/synthetic-api/drop?environment=4&pending=4&pending=3'
  )
})

test('outside a project page, an uploaded source gets an upload link and no redeploy that would be declined', async () => {
  const api = serveEnvironments('archive')
  const view = renderPrompt(undefined, 'uploaded_source')

  await view.findByText(/Upload the source again to apply the change now/)
  expect(view.queryByRole('button', { name: /Redeploy/ })).toBeNull()
  expect(
    view.getByRole('link', { name: /Upload source again for production/ })
  ).toBeTruthy()
  // The links that remain still work.
  expect(view.getByRole('link', { name: 'View deployments' })).toBeTruthy()
  // Reading the live deployments decided this; nothing was started.
  expect(api.calls.filter((call) => call.method !== 'GET')).toEqual([])
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

test('a manual project running an image keeps its Redeploy action', async () => {
  serveEnvironments('image')
  const view = renderPrompt(undefined, 'manual')

  const button = await enabledButton(view, /Redeploy now/)
  expect(button).toBeTruthy()
  expect(view.queryByText(/Upload the source again/)).toBeNull()
  expect(view.queryByRole('button', { name: /Upload source again/ })).toBeNull()
})
