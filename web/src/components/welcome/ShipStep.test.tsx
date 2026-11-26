// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { ShipStep } from './ShipStep'

installBrowserWindow()
afterEach(cleanup)

const project = (id: number, name: string) => ({ id, name, slug: name })

function renderStep() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/welcome']}>
        <Routes>
          <Route path="/welcome" element={<ShipStep />} />
          <Route path="/projects/new" element={<p>New project page</p>} />
          <Route path="/projects/:slug" element={<p>Project page</p>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, client }
}

function mock(status: 'connected' | 'missing', projects: () => unknown[]) {
  const patches: unknown[] = []
  const api = mockApi({
    'GET /api-keys/ai-harness/status': () =>
      jsonResponse({ status, reason: null, connected_via: null }),
    'GET /projects': () =>
      jsonResponse({ projects: projects(), page: 1, per_page: 20, total: 0 }),
    'PATCH /settings/onboarding': async (request) => {
      patches.push(await request.json())
      return jsonResponse({ done: [], later: [] })
    },
  })
  return { api, patches }
}

test('with an agent connected, it leads, and the operator opens the new project their agent made', async () => {
  let list = [project(1, 'existing')]
  const { api, patches } = mock('connected', () => list)
  const { view, client } = renderStep()

  const agent = await view.findByRole('radio', { name: /Ask your agent/ })
  await view.findByText('Recommended')
  expect(agent.getAttribute('aria-checked')).toBe('true')
  expect(
    view.getByRole('region', { name: 'Prompt for your agent' }).textContent
  ).toContain(
    `Help me put my first app live on my Temps server at ${window.location.origin}.`
  )
  await view.findByText('Waiting for a new project…')
  const open = view.getByRole('button', { name: 'Open the project' })
  expect(open.getAttribute('aria-disabled')).toBe('true')
  fireEvent.click(open)
  expect(patches).toEqual([])

  // A project that was already there is not offered; a new one is, without
  // claiming the agent made it.
  list = [project(2, 'my-app'), ...list]
  await act(() => client.refetchQueries())
  await view.findByText(/New since this step opened/)
  expect(view.queryByText(/Your agent created/)).toBeNull()
  expect(view.queryByRole('button', { name: 'Open existing' })).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'Open my-app' }))
  await view.findByText('Project page')
  expect(patches).toEqual([{ done: ['app'], completed: true }])
  api.restore()
})

test('a project another operator creates meanwhile is offered, not credited, and the step keeps watching', async () => {
  let list = [project(1, 'existing')]
  const { api, patches } = mock('connected', () => list)
  const { view, client } = renderStep()
  await view.findByText('Waiting for a new project…')

  list = [project(2, 'someone-elses'), ...list]
  await act(() => client.refetchQueries())
  await view.findByRole('button', { name: 'Open someone-elses' })
  // Nothing is completed until the operator opens one.
  expect(patches).toEqual([])

  // A later project still appears beside it, so the agent's is reachable.
  list = [project(3, 'agents-app'), ...list]
  await act(() => client.refetchQueries())
  fireEvent.click(await view.findByRole('button', { name: 'Open agents-app' }))
  await view.findByText('Project page')
  expect(patches).toEqual([{ done: ['app'], completed: true }])
  api.restore()
})

test('without an agent, New project leads and goes to its page', async () => {
  const { api, patches } = mock('missing', () => [])
  const { view } = renderStep()

  await view.findByText(/No agent is connected yet/)
  // Two ways only: the agent, or the New project page.
  expect(view.getAllByRole('radio')).toHaveLength(2)
  expect(
    view
      .getByRole('radio', { name: /A new project/ })
      .getAttribute('aria-checked')
  ).toBe('true')
  fireEvent.click(view.getByRole('button', { name: 'Go to New project' }))
  await view.findByText('New project page')
  // The wizard closes, but nothing is live yet, so the step stays open.
  expect(patches).toEqual([{ completed: true }])
  api.restore()
})
