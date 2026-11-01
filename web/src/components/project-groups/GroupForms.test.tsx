// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, within } from '@testing-library/react'
import type { ReactElement } from 'react'
import { MemoryRouter } from 'react-router'
import type { ProjectGroupResponse } from '@/api/client'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { GroupSettingsPanel } from './GroupSettingsPanel'
import { GroupVariablesPanel } from './GroupVariablesPanel'
import { ImportServicesDialog } from './ImportServicesDialog'

installBrowserWindow()
afterEach(cleanup)

const GROUP: ProjectGroupResponse = {
  id: 7,
  name: 'Shop',
  slug: 'shop',
  description: 'The storefront',
  projects: [],
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

function wrap(ui: ReactElement) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>{ui}</MemoryRouter>
    </QueryClientProvider>
  )
}

async function settle(ms = 30) {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms))
  })
}

async function change(element: Element | null, value: string) {
  if (!element) throw new Error('field not found')
  await act(async () => {
    fireEvent.change(element, { target: { value } })
  })
}

async function click(element: HTMLElement) {
  await act(async () => {
    fireEvent.click(element)
  })
  await settle()
}

function field(id: string): HTMLInputElement {
  const element = document.body.querySelector<HTMLInputElement>(`#${id}`)
  if (!element) throw new Error(`no field #${id}`)
  return element
}

test('settings: a blank name is refused, a changed slug is saved lowercase', async () => {
  const bodies: Record<string, unknown>[] = []
  const api = mockApi({
    'GET /project-groups/7/env-vars': () => jsonResponse([]),
    'PATCH /project-groups/7': async (request) => {
      bodies.push((await request.json()) as Record<string, unknown>)
      return jsonResponse({ ...GROUP, slug: 'storefront' })
    },
  })
  render(
    wrap(<GroupSettingsPanel group={GROUP} onConnectRepository={() => {}} />)
  )
  await settle()
  const view = within(document.body)

  // Nothing changed yet: nothing to save.
  const save = view.getByRole('button', { name: /^save$/i })
  expect((save as HTMLButtonElement).disabled).toBe(true)

  await change(field('group-settings-name'), '   ')
  await click(save)
  expect(view.getByText('Give the project a name.')).toBeTruthy()
  expect(api.called('PATCH', '/project-groups/7')).toBe(false)

  await change(field('group-settings-name'), ' Shop ')
  await change(field('group-settings-slug'), 'StoreFront')
  expect(field('group-settings-slug').value).toBe('storefront')
  await click(save)

  expect(bodies).toEqual([
    {
      name: 'Shop',
      slug: 'storefront',
      description: 'The storefront',
    },
  ])
  api.restore()
})

test('settings: the server error is shown and the edits are kept', async () => {
  const api = mockApi({
    'GET /project-groups/7/env-vars': () => jsonResponse([]),
    'PATCH /project-groups/7': () =>
      jsonResponse({ title: 'Conflict', detail: 'That address is taken' }, 409),
  })
  render(
    wrap(<GroupSettingsPanel group={GROUP} onConnectRepository={() => {}} />)
  )
  await settle()

  await change(field('group-settings-slug'), 'taken')
  await click(within(document.body).getByRole('button', { name: /^save$/i }))

  expect(within(document.body).getByRole('alert').textContent).toContain(
    'That address is taken'
  )
  expect(field('group-settings-slug').value).toBe('taken')
  api.restore()
})

test('variables: the name is uppercased and required, and is sent trimmed', async () => {
  const bodies: Record<string, unknown>[] = []
  const api = mockApi({
    'GET /project-groups/7/env-vars': () => jsonResponse([]),
    'POST /project-groups/7/env-vars': async (request) => {
      bodies.push((await request.json()) as Record<string, unknown>)
      return jsonResponse({
        id: 1,
        key: 'API_URL',
        value: 'https://api.example.test',
        is_secret: false,
        include_in_preview: false,
        environment_slug: null,
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
      })
    },
  })
  render(wrap(<GroupVariablesPanel group={GROUP} />))
  await settle()
  const view = within(document.body)

  await click(view.getAllByRole('button', { name: /add/i })[0])
  const submit = view.getByRole('button', { name: 'Add variable' })
  // No name yet: nothing to submit.
  expect((submit as HTMLButtonElement).disabled).toBe(true)

  await change(field('group-var-key'), ' api_url ')
  expect(field('group-var-key').value).toBe(' API_URL ')
  await change(field('group-var-value'), 'https://api.example.test')
  await click(submit)

  expect(bodies).toEqual([
    {
      key: 'API_URL',
      value: 'https://api.example.test',
      is_secret: false,
      environment_slug: null,
      include_in_preview: false,
    },
  ])
  api.restore()
})

test('variables: the server error is shown inside the dialog', async () => {
  const api = mockApi({
    'GET /project-groups/7/env-vars': () => jsonResponse([]),
    'POST /project-groups/7/env-vars': () =>
      jsonResponse({ title: 'Bad', detail: 'API_URL already exists' }, 409),
  })
  render(wrap(<GroupVariablesPanel group={GROUP} />))
  await settle()
  const view = within(document.body)

  await click(view.getAllByRole('button', { name: /add/i })[0])
  await change(field('group-var-key'), 'API_URL')
  await click(view.getByRole('button', { name: 'Add variable' }))

  expect(
    view
      .getAllByRole('alert')
      .some((alert) => alert.textContent?.includes('API_URL already exists'))
  ).toBe(true)
  api.restore()
})

const IMPORT_GROUP: ProjectGroupResponse = {
  ...GROUP,
  repository: {
    repo_owner: 'acme',
    repo_name: 'shop',
    main_branch: 'main',
    public_provider: 'github',
  },
}

const IMPORT_ROUTES = {
  'GET /project-groups/7/repository': () =>
    jsonResponse({
      repository: {
        repo_owner: 'acme',
        repo_name: 'shop',
        main_branch: 'main',
        public_provider: 'github',
      },
    }),
  'GET /git/public/github/acme/shop/presets': () =>
    jsonResponse({
      branch: 'main',
      presets: [
        {
          path: 'apps/web',
          preset: 'nextjs',
          preset_label: 'Next.js',
          project_type: 'server',
        },
        {
          path: 'apps/api',
          preset: 'nodejs',
          preset_label: 'Node.js',
          project_type: 'server',
        },
      ],
    }),
}

function nameInputFor(directory: string): HTMLInputElement {
  const input = document.body.querySelector<HTMLInputElement>(
    `input[aria-label="Service name for ${directory}"]`
  )
  if (!input) throw new Error(`no name field for ${directory}`)
  return input
}

test('import: names are suggested, and a blank one is refused before anything is sent', async () => {
  const api = mockApi({
    ...IMPORT_ROUTES,
    'POST /project-groups/7/import': () => jsonResponse({}, 202),
  })
  render(
    wrap(
      <ImportServicesDialog group={IMPORT_GROUP} open onOpenChange={() => {}} />
    )
  )
  await settle(60)
  const view = within(document.body)

  expect(nameInputFor('apps/web').value).toBe('web')
  expect(nameInputFor('apps/api').value).toBe('api')

  await change(nameInputFor('apps/api'), '  ')
  await click(view.getByRole('button', { name: /add \d+ services?/i }))

  expect(view.getByText('Give the service a name.')).toBeTruthy()
  expect(api.called('POST', '/project-groups/7/import')).toBe(false)
  api.restore()
})
