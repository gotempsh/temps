// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
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
import { toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  ProjectDeployContext,
  type OpenDeployOptions,
} from './project-deploy-context'
import { ServiceLinkRedeployPrompt } from './ServiceLinkRedeployPrompt'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'warning').mockRestore()
})

// Other sources on: a Git project that took an uploaded archive can take it
// again. The tests that turn it off say so.
const project = {
  id: 7,
  slug: 'synthetic-api',
  source_type: 'git',
  allow_alternate_sources: true,
} as const
const environment = (id: number, slug: string, deployment: number) => ({
  id,
  slug,
  name: slug,
  is_preview: false,
  current_deployment_id: deployment,
  branch: slug === 'production' ? 'main' : slug,
})

/** What each environment's live deployment was made from. */
type Made = 'git' | 'image' | 'archive' | 'manual'
const deploymentOf = (id: number, made: Made) => ({
  id,
  ...(made === 'git' ? { branch: 'main', commit_hash: 'abc123' } : {}),
  ...(made === 'image' ? { metadata: { externalImageRef: 'app:1' } } : {}),
  // A deployment made from an uploaded archive says so itself, whatever the
  // project's own source type is.
  ...(made === 'archive'
    ? { metadata: { deploymentSourceType: 'uploaded_source' } }
    : {}),
  // Nothing to rebuild and nothing to upload.
  ...(made === 'manual'
    ? { metadata: { deploymentSourceType: 'manual' } }
    : {}),
})

/**
 * Two deployed environments, production and staging, made from `production`
 * and `staging` respectively; records every request the prompt sends.
 */
function serveEnvironments(made: Made = 'git', staging: Made = made) {
  return mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([
        environment(4, 'staging', 41),
        environment(3, 'production', 31),
      ]),
    'GET /projects/7/deployments/41': () =>
      jsonResponse(deploymentOf(41, staging)),
    'GET /projects/7/deployments/31': () =>
      jsonResponse(deploymentOf(31, made)),
  })
}

/** The prompt on a project page, recording each deploy dialog it opens. */
function renderOnProjectPage(opened: OpenDeployOptions[]) {
  return renderPrompt((children) => (
    <ProjectDeployContext.Provider
      value={{ openDeploy: (options) => opened.push(options ?? {}) }}
    >
      {children}
    </ProjectDeployContext.Provider>
  ))
}

/** Waits for the last "Redeploy now" button to render enabled, then clicks it. */
async function clickRedeployNow(view: ReturnType<typeof renderPrompt>) {
  // Re-query until the final button is there: the first one rendered, before
  // the environments load, is replaced by the per-environment buttons and then
  // by this one.
  await waitFor(() =>
    expect(
      (view.getByRole('button', { name: /Redeploy now/ }) as HTMLButtonElement)
        .disabled
    ).toBe(false)
  )
  fireEvent.click(view.getByRole('button', { name: /Redeploy now/ }))
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
  sourceType: 'git' | 'uploaded_source' | 'manual' = project.source_type,
  allowAlternateSources: boolean = project.allow_alternate_sources
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
            project={{
              ...project,
              source_type: sourceType,
              allow_alternate_sources: allowAlternateSources,
            }}
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
  // (held back for a moment while what is left is looked up again)
  fireEvent.click(await enabledButton(view, /Redeploy now/))
  expect(opened[1].environmentId).toBe(4)
  expect(opened[1].onStarted).toBeUndefined()
})

// One environment runs Git, the other an uploaded archive. While both wait,
// a Redeploy is offered (one of them can use it); once the Git one is started
// the prompt must follow what is left, or it keeps offering a Redeploy the
// archive would decline.
test('after the redeployable environment is started, the archive left behind is sent to the upload page', async () => {
  mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([
        environment(4, 'staging', 41),
        environment(3, 'production', 31),
      ]),
    'GET /projects/7/deployments/31': () =>
      jsonResponse(deploymentOf(31, 'git')),
    'GET /projects/7/deployments/41': () =>
      jsonResponse(deploymentOf(41, 'archive')),
  })
  const opened: OpenDeployOptions[] = []
  const view = renderPrompt((children) => (
    <ProjectDeployContext.Provider
      value={{ openDeploy: (options) => opened.push(options ?? {}) }}
    >
      {children}
    </ProjectDeployContext.Provider>
  ))

  fireEvent.click(await enabledButton(view, /Redeploy production/))
  act(() => opened[0].onStarted?.(3))

  const upload = await view.findByRole('link', {
    name: /Upload source again/,
  })
  expect(upload.getAttribute('href')).toBe(
    '/projects/synthetic-api/drop?environment=4'
  )
  expect(view.queryByRole('button', { name: /Redeploy/ })).toBeNull()
})

// The deploy dialog would offer the project's default source and say nothing of
// why the running deployment cannot be redeployed; the redeploy itself does.
test('on a project page, deployments with no source go straight to the redeploy, not the deploy dialog', async () => {
  const warning = spyOn(toast, 'warning').mockImplementation(() => 'id')
  const api = serveEnvironments('manual')
  const opened: OpenDeployOptions[] = []
  const view = renderOnProjectPage(opened)

  await clickRedeployNow(view)
  // The user is told, for each environment, why nothing was redeployed.
  await waitFor(() => expect(warning).toHaveBeenCalledTimes(1))
  const message = String(warning.mock.calls[0][0])
  expect(message).toContain(
    'staging not redeployed: it has no reusable source to rebuild from'
  )
  expect(message).toContain(
    'production not redeployed: it has no reusable source to rebuild from'
  )
  expect(opened).toEqual([])
  expect(api.calls.filter((call) => call.method !== 'GET')).toEqual([])
  // Not sent to an upload they never made.
  expect(view.queryByRole('link', { name: /Upload source again/ })).toBeNull()
})

// Neither an uploaded archive nor a manual deployment can be rebuilt in place,
// so a deploy dialog would offer the project's Git branch instead of what is
// running. The redeploy says what to do for each one.
test('on a project page, a mix of an uploaded archive and a deployment with no source goes to the redeploy, not the deploy dialog', async () => {
  const warning = spyOn(toast, 'warning').mockImplementation(() => 'id')
  const api = serveEnvironments('archive', 'manual')
  const opened: OpenDeployOptions[] = []
  const view = renderOnProjectPage(opened)

  await clickRedeployNow(view)
  await waitFor(() => expect(warning).toHaveBeenCalledTimes(1))
  const message = String(warning.mock.calls[0][0])
  expect(message).toContain(
    'production not redeployed: it was deployed from an uploaded archive; upload it again'
  )
  expect(message).toContain(
    'staging not redeployed: it has no reusable source to rebuild from'
  )
  expect(opened).toEqual([])
  expect(api.calls.filter((call) => call.method !== 'GET')).toEqual([])
})

// Production runs Git and is started through the dialog; staging, left
// behind, has no source. Redeploying what is left must not deploy production
// a second time, from its old commit.
test('an environment started through the deploy dialog is not redeployed again with the ones left', async () => {
  spyOn(toast, 'warning').mockImplementation(() => 'id')
  const api = serveEnvironments('git', 'manual')
  const opened: OpenDeployOptions[] = []
  const view = renderOnProjectPage(opened)

  fireEvent.click(await enabledButton(view, /Redeploy production/))
  act(() => opened[0].onStarted?.(3))

  await clickRedeployNow(view)
  // Staging's deployment is read for the redeploy itself, after the lookup.
  await waitFor(() =>
    expect(
      api.calls.filter((call) => call.path === '/projects/7/deployments/41')
        .length
    ).toBeGreaterThan(1)
  )
  await waitFor(() => expect(toast.warning).toHaveBeenCalled())
  // Production was only ever read by the first lookup, and nothing was
  // started for it outside the dialog.
  expect(
    api.calls.filter((call) => call.path === '/projects/7/deployments/31')
  ).toHaveLength(1)
  expect(api.calls.filter((call) => call.method !== 'GET')).toEqual([])
  expect(opened).toHaveLength(1)
})

test("while each environment's source is looked up, the held-back action shows it is busy", async () => {
  let answer: (response: Response) => void = () => {}
  mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([environment(3, 'production', 31)]),
    'GET /projects/7/deployments/31': () =>
      new Promise<Response>((resolve) => {
        answer = resolve
      }),
  })
  const view = renderPrompt()

  await waitFor(() =>
    expect(
      view
        .getByRole('button', { name: /Redeploy now/ })
        .getAttribute('aria-busy')
    ).toBe('true')
  )
  const button = view.getByRole('button', {
    name: /Redeploy now/,
  }) as HTMLButtonElement
  expect(button.disabled).toBe(true)
  expect(button.querySelector('.animate-spin')).not.toBeNull()

  answer(jsonResponse(deploymentOf(31, 'git')))
  await waitFor(() => expect(button.disabled).toBe(false))
  expect(button.getAttribute('aria-busy')).toBe('false')
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
  // A long environment name wraps instead of widening the page.
  expect(staging.className).toContain('whitespace-normal')
  expect(staging.className).toContain('max-w-full')
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

// The upload page would be refused: the server takes an archive into a Git
// project only while Other sources is on.
test('a Git project with Other sources off is sent to that setting, not to an upload it would refuse', async () => {
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
    'git',
    false
  )

  const setting = await view.findByRole('link', {
    name: /Open Other sources settings/,
  })
  expect(setting.getAttribute('href')).toBe(
    '/projects/synthetic-api/settings/delivery?section=source#setting-alternate-sources'
  )
  expect(view.getByText(/accepts only while Other sources is on/)).toBeTruthy()
  expect(view.queryByRole('link', { name: /Upload source again/ })).toBeNull()
  expect(view.queryByRole('button', { name: /Redeploy/ })).toBeNull()
  fireEvent.click(setting)
  expect(opened).toEqual([])
})

test('an uploaded-source project keeps its upload link whatever Other sources says', async () => {
  serveEnvironments('archive')
  const view = renderPrompt(undefined, 'uploaded_source', false)

  expect(
    await view.findByRole('link', { name: /Upload source again for staging/ })
  ).toBeTruthy()
  expect(
    view.queryByRole('link', { name: /Open Other sources settings/ })
  ).toBeNull()
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
