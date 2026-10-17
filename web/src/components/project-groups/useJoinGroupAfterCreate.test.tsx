// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render, within } from '@testing-library/react'
import { MemoryRouter, useLocation } from 'react-router'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import {
  parseTargetGroupId,
  useJoinGroupAfterCreate,
} from './useJoinGroupAfterCreate'

installBrowserWindow()
afterEach(cleanup)

const SERVICE = { id: 5, slug: 'web', name: 'Web' }
const GROUP = {
  id: 7,
  name: 'Shop',
  slug: 'shop',
  description: null,
  projects: [],
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
}

/**
 * A creation flow that deploys: join first, deploy second, as
 * ManualProjectConfigurator does it. `events` records what happened in order.
 */
function Flow({ events }: { events: string[] }) {
  const { joinBeforeDeploy, continueTo, joinGroupDialog } =
    useJoinGroupAfterCreate()
  const location = useLocation()

  const start = async () => {
    const deploy = async () => {
      events.push('deploy')
      continueTo('/projects/web/deployments')
    }
    if (!(await joinBeforeDeploy(SERVICE, deploy))) return
    await deploy()
  }

  return (
    <>
      <button onClick={() => void start()}>create</button>
      <p data-testid="path">{location.pathname}</p>
      {joinGroupDialog}
    </>
  )
}

function page(events: string[], url: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[url]}>
        <Flow events={events} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function pathOf(view: ReturnType<typeof render>) {
  return view.getByTestId('path').textContent
}

async function click(element: HTMLElement) {
  await act(async () => {
    fireEvent.click(element)
  })
  // Let the awaited join, invalidation and navigation settle.
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20))
  })
}

test('joins the target project before the deployment starts', async () => {
  const events: string[] = []
  const api = mockApi({
    'GET /project-groups/7': () => jsonResponse(GROUP),
    'PUT /project-groups/7/projects/5': () => {
      events.push('join')
      return jsonResponse(GROUP)
    },
  })
  const view = render(page(events, '/projects/new?group=7'))

  await click(view.getByText('create'))

  expect(events).toEqual(['join', 'deploy'])
  expect(pathOf(view)).toBe('/p/shop')
  api.restore()
})

test('starts no deployment when joining fails, and deploys after a successful retry', async () => {
  const events: string[] = []
  let joinAttempts = 0
  const api = mockApi({
    'GET /project-groups/7': () => jsonResponse(GROUP),
    'PUT /project-groups/7/projects/5': () => {
      joinAttempts += 1
      events.push('join')
      if (joinAttempts === 1) {
        return jsonResponse({ title: 'Boom', detail: 'database is down' }, 500)
      }
      return jsonResponse(GROUP)
    },
  })
  const view = render(page(events, '/projects/new?group=7'))

  await click(view.getByText('create'))

  // The failure is surfaced, nothing was deployed, the user stays put.
  expect(events).toEqual(['join'])
  expect(pathOf(view)).toBe('/projects/new')
  const body = within(document.body)
  expect(body.getByText(/Web was created, but not added to Shop/)).toBeTruthy()
  expect(body.getByText(/Nothing was deployed/)).toBeTruthy()

  await click(body.getByRole('button', { name: /retry/i }))

  expect(events).toEqual(['join', 'join', 'deploy'])
  expect(pathOf(view)).toBe('/p/shop')
  api.restore()
})

test('deploys straight away and never calls the API without a target project', async () => {
  const events: string[] = []
  const api = mockApi({})
  const view = render(page(events, '/projects/new'))

  await click(view.getByText('create'))

  expect(events).toEqual(['deploy'])
  expect(pathOf(view)).toBe('/projects/web/deployments')
  expect(api.calls.some((call) => call.method === 'PUT')).toBe(false)
  api.restore()
})

test('parses only positive integer group ids', () => {
  expect(parseTargetGroupId('7')).toBe(7)
  expect(parseTargetGroupId('0')).toBeNull()
  expect(parseTargetGroupId('abc')).toBeNull()
  expect(parseTargetGroupId(null)).toBeNull()
})
