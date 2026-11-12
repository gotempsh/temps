// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { getAutofixCapabilityOptions } from '@/api/client/@tanstack/react-query.gen'
import { client } from '@/api/client/client.gen'
import { restoreClientConfig } from '@/test/client-config'
import type {
  AutofixCapabilityResponse,
  AutofixSettingsResponse,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { AutofixSettingsCard } from './AutofixSettingsCard'

installBrowserWindow()

const originalConfig = client.getConfig()

afterEach(() => {
  cleanup()
  restoreClientConfig(client, originalConfig)
})

const PROJECT = 7
const OTHER_PROJECT = 8

/**
 * A tiny autofix backend: the capability read returns whatever the last
 * write left behind, so a pause toggle or a save is followed by a refetch
 * that hands the card a fresh `settings` object, as the real server does.
 * Each project keeps its own settings.
 */
function serve() {
  const stored = new Map<number, AutofixSettingsResponse>(
    [PROJECT, OTHER_PROJECT].map((project_id) => [
      project_id,
      {
        project_id,
        mode: 'off',
        min_confidence: 4,
        paused: false,
        protected_paths: [],
        test_command: project_id === OTHER_PROJECT ? 'make check' : null,
      },
    ])
  )
  const writes: Array<{ path: string; body: unknown }> = []
  const capability = (
    settings: AutofixSettingsResponse
  ): AutofixCapabilityResponse => ({
    configured: true,
    delivery: 'pull_request',
    source_kind: 'github',
    settings,
  })
  client.setConfig({
    baseUrl: 'https://console.example.test/api',
    fetch: Object.assign(
      async (input: RequestInfo | URL) => {
        const request = input instanceof Request ? input : new Request(input)
        const path = new URL(request.url).pathname
        const match = /^\/api\/projects\/(\d+)\/autofix(\/\w+)?$/.exec(path)
        const settings = match ? stored.get(Number(match[1])) : undefined
        if (!match || !settings) return new Promise<Response>(() => {})
        const save = (next: AutofixSettingsResponse) => {
          stored.set(next.project_id, next)
          return Response.json(next)
        }
        if (match[2] === undefined) {
          return Response.json(capability(settings))
        }
        if (match[2] === '/pause') {
          const body = (await request.json()) as { paused: boolean }
          writes.push({ path, body })
          return save({ ...settings, paused: body.paused })
        }
        if (match[2] === '/settings') {
          const body = (await request.json()) as {
            mode: string
            min_confidence: number
            protected_paths: string[]
            test_command: string
          }
          writes.push({ path, body })
          return save({ ...settings, ...body })
        }
        return new Promise<Response>(() => {})
      },
      { preconnect: fetch.preconnect }
    ),
  })
  return writes
}

function renderCard(queries = newQueryClient()) {
  const tree = (projectId: number) => (
    <QueryClientProvider client={queries}>
      <MemoryRouter>
        <AutofixSettingsCard projectId={projectId} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const view = render(tree(PROJECT))
  return { ...view, showProject: (id: number) => view.rerender(tree(id)) }
}

function newQueryClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } })
}

test('pausing autofix keeps the policy edits that are not saved yet', async () => {
  const writes = serve()
  renderCard()

  const command = (await screen.findByPlaceholderText(
    'npm test'
  )) as HTMLInputElement
  fireEvent.change(command, { target: { value: 'bun test' } })
  const paths = screen.getByRole('textbox', {
    name: 'Protected paths',
  }) as HTMLTextAreaElement
  fireEvent.change(paths, { target: { value: 'migrations/**' } })

  const toggle = screen.getByRole('switch', { name: 'Pause autofix' })
  fireEvent.click(toggle)

  // The pause round-trips and the capability is read again.
  await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe('true'))
  expect(writes.map((w) => w.path)).toEqual([
    `/api/projects/${PROJECT}/autofix/pause`,
  ])
  expect(command.value).toBe('bun test')
  expect(paths.value).toBe('migrations/**')
})

test('the modes stack in one column, at every width, so their details stay readable', async () => {
  serve()
  renderCard()

  const modes = await screen.findByRole('radiogroup')

  expect(screen.getAllByRole('radio')).toHaveLength(3)
  expect(modes.className).toContain('grid-cols-1')
  expect(modes.className).not.toMatch(/grid-cols-[2-9]/)
})

test('saving makes the saved values the form baseline', async () => {
  const writes = serve()
  renderCard()

  const command = (await screen.findByPlaceholderText(
    'npm test'
  )) as HTMLInputElement
  fireEvent.change(command, { target: { value: '  bun test  ' } })
  fireEvent.click(screen.getByRole('button', { name: 'Save' }))

  // The trimmed value the server stored replaces what was typed.
  await waitFor(() => expect(command.value).toBe('bun test'))
  expect(writes).toHaveLength(1)
  expect(writes[0]?.body).toMatchObject({ test_command: 'bun test' })

  // A later refetch (here, after pausing) leaves the saved value in place.
  const toggle = screen.getByRole('switch', { name: 'Pause autofix' })
  fireEvent.click(toggle)
  await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe('true'))
  expect(command.value).toBe('bun test')
})

test('switching to another cached project drops the unsaved edits', async () => {
  const writes = serve()
  const queries = newQueryClient()
  // The other project's settings are already in the cache, so the card gets
  // them on the very first render after the switch.
  await queries.prefetchQuery(
    getAutofixCapabilityOptions({ path: { project_id: OTHER_PROJECT } })
  )
  const { showProject } = renderCard(queries)

  const command = (await screen.findByPlaceholderText(
    'npm test'
  )) as HTMLInputElement
  fireEvent.change(command, { target: { value: 'rm -rf build' } })
  const paths = screen.getByRole('textbox', {
    name: 'Protected paths',
  }) as HTMLTextAreaElement
  fireEvent.change(paths, { target: { value: 'secrets/**' } })

  showProject(OTHER_PROJECT)

  // The form shows the other project's own values, not the edits made for
  // the first one, and saving writes only those.
  await waitFor(() => expect(command.value).toBe('make check'))
  expect(paths.value).toBe('')
  fireEvent.click(screen.getByRole('button', { name: 'Save' }))
  await waitFor(() => expect(writes).toHaveLength(1))
  expect(writes[0]?.path).toBe(
    `/api/projects/${OTHER_PROJECT}/autofix/settings`
  )
  expect(writes[0]?.body).toMatchObject({
    test_command: 'make check',
    protected_paths: [],
  })
})
