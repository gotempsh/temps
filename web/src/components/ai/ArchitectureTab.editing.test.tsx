// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { client } from '@/api/client/client.gen'
import { restoreApiClient } from '@/test/api-client'
import type {
  BlueprintDocument,
  BlueprintResponse,
  ChatWorkspaceResponse,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { ArchitectureTab } from './ArchitectureTab'

installBrowserWindow()

const originalConfig = client.getConfig()

afterEach(() => {
  cleanup()
  restoreApiClient(originalConfig)
})

const CHAT = 'conv_1'
const APP = 'app_1'

const noApplication: ChatWorkspaceResponse = { chats: [], joinable: true }
const inApplication: ChatWorkspaceResponse = {
  workspace: { public_id: APP, name: 'Team tasks' },
  chats: [],
  joinable: true,
}
const emptyBlueprint: BlueprintResponse = {
  application_id: APP,
  workspace_name: 'Team tasks',
  accepted: null,
  proposal: null,
  bindings: {},
  live: {},
}

function deferred<T>() {
  let resolve: (value: T) => void = () => {}
  const promise = new Promise<T>((r) => {
    resolve = r
  })
  return { promise, resolve }
}

/**
 * The chat starts with no application. Creating one answers at once; the
 * blueprint read is held until the test releases it, which is the window in
 * which the tab used to swap the editor for a skeleton.
 */
function serve(saveVersion: (document: BlueprintDocument) => Response) {
  let created = false
  const blueprintRead = deferred<void>()
  const saved: BlueprintDocument[] = []
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async (input: RequestInfo | URL) => {
        const request = input instanceof Request ? input : new Request(input)
        const path = new URL(request.url).pathname
        if (path === `/api/ai/conversations/${CHAT}/workspace`) {
          if (request.method === 'PUT') created = true
          return Response.json(created ? inApplication : noApplication)
        }
        if (path === `/api/ai/applications/${APP}/blueprint`) {
          await blueprintRead.promise
          return Response.json(emptyBlueprint)
        }
        if (path === `/api/ai/applications/${APP}/blueprint/versions`) {
          if (request.method !== 'POST') return Response.json([])
          const body = (await request.json()) as {
            document: BlueprintDocument
          }
          saved.push(body.document)
          return saveVersion(body.document)
        }
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
  return { saved, releaseBlueprint: () => blueprintRead.resolve() }
}

function renderTab() {
  const queries = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={queries}>
      <MemoryRouter>
        <ArchitectureTab conversationPublicId={CHAT} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/**
 * The text input showing `value`. Part names also appear in the native
 * `<select>` behind each flow's From/To picker, so match inputs only.
 */
function textInput(value: string): HTMLInputElement | null {
  return (
    screen
      .queryAllByDisplayValue(value)
      .find((el): el is HTMLInputElement => el instanceof HTMLInputElement) ??
    null
  )
}

/** Draw a design that differs from the starter, then save it. */
async function drawAndSave() {
  fireEvent.click(await screen.findByRole('button', { name: /Draw it myself/ }))
  fireEvent.change(screen.getByLabelText('Application name'), {
    target: { value: 'Team tasks' },
  })
  fireEvent.change(
    screen.getByLabelText('What the application does, and for whom'),
    { target: { value: 'Tasks for small teams' } }
  )
  const webApp = textInput('Web app')
  if (!webApp) throw new Error('The starter design has no Web app part')
  fireEvent.change(webApp, { target: { value: 'Task board' } })
  fireEvent.change(screen.getByLabelText('What you changed'), {
    target: { value: 'First design' },
  })
  fireEvent.click(screen.getByRole('button', { name: /Save as a new version/ }))
}

test('a refused first save keeps the drawing the errors describe', async () => {
  const backend = serve(() =>
    Response.json(
      {
        title: 'Invalid blueprint',
        status: 400,
        problems: ['Task board has no flow to a data part'],
      },
      { status: 400, headers: { 'Content-Type': 'application/problem+json' } }
    )
  )
  renderTab()
  await drawAndSave()

  // The save is refused while the new application's blueprint is still
  // loading: the editor is still there, with the user's drawing.
  await screen.findByText('Task board has no flow to a data part')
  expect(backend.saved[0]?.components.map((c) => c.name)).toContain(
    'Task board'
  )
  expect(textInput('Task board')).not.toBeNull()
  expect(screen.getByDisplayValue('Tasks for small teams')).toBeTruthy()

  // And once the blueprint arrives it still is.
  await act(async () => {
    backend.releaseBlueprint()
    await new Promise((resolve) => setTimeout(resolve, 20))
  })
  expect(screen.queryByLabelText('Application name')).toBeNull()
  expect(textInput('Task board')).not.toBeNull()
  expect(screen.getByDisplayValue('First design')).toBeTruthy()
  expect(screen.getByText('Task board has no flow to a data part')).toBeTruthy()
  expect(textInput('Web app')).toBeNull()
})

test('a saved first design closes the editor and shows the version', async () => {
  const backend = serve((document) =>
    Response.json(
      {
        ...emptyBlueprint,
        accepted: {
          version: 1,
          document,
          change_summary: 'First design',
          status: 'accepted',
          proposed_by_assistant: false,
          created_at: '2026-10-20T08:00:00Z',
          decided_at: '2026-10-20T08:00:00Z',
        },
      } satisfies BlueprintResponse,
      { status: 201 }
    )
  )
  renderTab()
  await drawAndSave()

  await screen.findByText(/version 1/)
  expect(screen.queryByRole('button', { name: /Save as a new version/ })).toBe(
    null
  )
  expect(screen.getByText('Tasks for small teams')).toBeTruthy()
  backend.releaseBlueprint()
})
