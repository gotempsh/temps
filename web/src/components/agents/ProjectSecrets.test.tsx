// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { Toaster, toast } from 'sonner'
import type { SecretResponse } from '@/api/client/types.gen'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { AgentSecrets } from './ProjectSecrets'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

// One name in two scopes: the case where a name-only request hits the wrong
// secret.
function secret(id: number, project_id: number | null): SecretResponse {
  return {
    id,
    project_id,
    name: 'API_TOKEN',
    secret_type: 'env',
    value: '***',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
  }
}
const SECRETS = [secret(1, null), secret(2, 7)]

function listResponse() {
  return jsonResponse({ items: SECRETS, total: SECRETS.length })
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AgentSecrets />
        <Toaster />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('each row shows the scope it belongs to', async () => {
  const api = mockApi({ 'GET /settings/secrets': listResponse })
  const view = renderPage()
  expect(await view.findByText('Instance-wide')).toBeTruthy()
  expect(view.getByText('Project #7')).toBeTruthy()
  api.restore()
})

test('deleting the project row deletes that project scope, not the instance-wide secret', async () => {
  const deleted: string[] = []
  const api = mockApi({
    'GET /settings/secrets': listResponse,
    'DELETE /settings/secrets/API_TOKEN': (request) => {
      deleted.push(new URL(request.url).search)
      return new Response(null, { status: 204 })
    },
  })
  const view = renderPage()
  await act(async () => {
    fireEvent.click(await view.findByLabelText('Delete API_TOKEN (Project #7)'))
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  await waitFor(() => expect(deleted).toEqual(['?project_id=7']))
  api.restore()
})

test('deleting the instance-wide row sends no project scope', async () => {
  const deleted: string[] = []
  const api = mockApi({
    'GET /settings/secrets': listResponse,
    'DELETE /settings/secrets/API_TOKEN': (request) => {
      deleted.push(new URL(request.url).search)
      return new Response(null, { status: 204 })
    },
  })
  const view = renderPage()
  await act(async () => {
    fireEvent.click(
      await view.findByLabelText('Delete API_TOKEN (Instance-wide)')
    )
  })
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  await waitFor(() => expect(deleted).toEqual(['']))
  api.restore()
})

test('editing the project row keeps the secret in its project scope', async () => {
  const bodies: unknown[] = []
  const api = mockApi({
    'GET /settings/secrets': listResponse,
    'POST /settings/secrets': async (request) => {
      bodies.push(await request.json())
      return jsonResponse(SECRETS[1], 201)
    },
  })
  const view = renderPage()
  await act(async () => {
    fireEvent.click(await view.findByLabelText('Edit API_TOKEN (Project #7)'))
  })
  await act(async () => {
    fireEvent.change(await view.findByLabelText('Value'), {
      target: { value: 'rotated-value' },
    })
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save' }))
  })
  await waitFor(() => expect(bodies.length).toBe(1))
  expect(bodies[0]).toMatchObject({
    name: 'API_TOKEN',
    project_id: 7,
    value: 'rotated-value',
  })
  api.restore()
})

function projectsResponse() {
  return jsonResponse({
    projects: [{ id: 3, slug: 'shop', name: 'Shop' }],
    total: 1,
    page: 1,
    per_page: 100,
  })
}

async function fillNewSecret(view: ReturnType<typeof renderPage>) {
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: /^Add secret/ }))
  })
  await act(async () => {
    fireEvent.change(await view.findByLabelText('Name'), {
      target: { value: 'DB_PASSWORD' },
    })
    fireEvent.change(view.getByLabelText('Value'), {
      target: { value: 'new-value' },
    })
  })
}

test('a new secret can be scoped to one project', async () => {
  const bodies: unknown[] = []
  const api = mockApi({
    'GET /settings/secrets': listResponse,
    'GET /projects': projectsResponse,
    'POST /settings/secrets': async (request) => {
      bodies.push(await request.json())
      return jsonResponse(SECRETS[1], 201)
    },
  })
  const view = renderPage()
  await fillNewSecret(view)
  await act(async () => {
    fireEvent.click(view.getByRole('combobox', { name: 'Scope' }))
  })
  await act(async () => {
    fireEvent.click(await view.findByText('Shop'))
  })
  expect(
    await view.findByText("Injected only into this project's agents.", {
      exact: false,
    })
  ).toBeTruthy()
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save Secret' }))
  })
  await waitFor(() => expect(bodies.length).toBe(1))
  expect(bodies[0]).toMatchObject({ name: 'DB_PASSWORD', project_id: 3 })
  api.restore()
})

test('a new secret left on the default scope is instance-wide', async () => {
  const bodies: Record<string, unknown>[] = []
  const api = mockApi({
    'GET /settings/secrets': listResponse,
    'GET /projects': projectsResponse,
    'POST /settings/secrets': async (request) => {
      bodies.push(await request.json())
      return jsonResponse(SECRETS[0], 201)
    },
  })
  const view = renderPage()
  await fillNewSecret(view)
  expect(view.getByRole('combobox', { name: 'Scope' }).textContent).toContain(
    'Instance-wide (every project)'
  )
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save Secret' }))
  })
  await waitFor(() => expect(bodies.length).toBe(1))
  expect(bodies[0].name).toBe('DB_PASSWORD')
  expect('project_id' in bodies[0]).toBe(false)
  api.restore()
})

test('editing shows the scope but explains how to move the secret instead', async () => {
  const api = mockApi({
    'GET /settings/secrets': listResponse,
    'GET /projects': projectsResponse,
  })
  const view = renderPage()
  await act(async () => {
    fireEvent.click(
      await view.findByLabelText('Edit API_TOKEN (Instance-wide)')
    )
  })
  const scope = await view.findByRole('combobox', { name: 'Scope' })
  expect(scope.hasAttribute('disabled')).toBe(true)
  expect(
    view.getByText('add it again in the new scope, then delete this one', {
      exact: false,
    })
  ).toBeTruthy()
  api.restore()
})
