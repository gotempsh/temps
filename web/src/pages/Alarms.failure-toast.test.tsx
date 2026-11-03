// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { toast } from 'sonner'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { Alarms } from './Alarms'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const REASON = 'Missing permission AlarmsWrite'
const PROBLEM = { title: 'Forbidden', detail: REASON }

const ALARM = {
  id: 9,
  project_id: 1,
  alarm_type: 'container_restart',
  severity: 'critical',
  status: 'firing',
  title: 'Container restarting',
  message: 'boom',
  fired_at: '2026-11-01T10:00:00Z',
  created_at: '2026-11-01T10:00:00Z',
  // Far enough ahead that the alarm counts as silenced whenever this runs.
  silenced_until: '2999-01-01T00:00:00Z',
  metadata: {},
}

function setup(route: string, answer: () => Response) {
  const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
  mockApi({
    'GET /projects': () =>
      jsonResponse({
        projects: [{ id: 1, slug: 'demo', name: 'demo' }],
        total: 1,
        page: 1,
        per_page: 100,
      }),
    'GET /projects/1/alarms/summary': () =>
      jsonResponse({
        acknowledged: 0,
        by_type: { container_restart: 1 },
        critical: 1,
        firing: 1,
        total_active: 1,
        warning: 0,
      }),
    'GET /projects/1/alarms': () =>
      jsonResponse({ items: [ALARM], page: 1, page_size: 25, total: 1 }),
    [route]: answer,
  })
  render(
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
      <BreadcrumbProvider>
        <MemoryRouter>
          <Alarms embedded />
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
  return failure
}

async function click(name: RegExp) {
  const target = await screen.findByRole('button', { name })
  await act(async () => {
    fireEvent.click(target)
  })
}

async function chooseSilenceItem(name: string) {
  const trigger = await screen.findByRole('button', { name: /^Silence/ })
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
  const item = await screen.findByRole('menuitem', { name })
  await act(async () => {
    fireEvent.click(item)
  })
}

const ACTIONS = [
  ['acknowledge', 'POST', 'acknowledge', () => click(/^Ack$/)],
  ['resolve', 'POST', 'resolve', () => click(/^Resolve$/)],
  ['silence', 'POST', 'silence', () => chooseSilenceItem('1 hour')],
  ['unsilence', 'DELETE', 'silence', () => chooseSilenceItem('Unsilence now')],
] as const

for (const [verb, method, path, perform] of ACTIONS) {
  test(`a refused ${verb} shows the server's reason`, async () => {
    const failure = setup(`${method} /projects/1/alarms/9/${path}`, () =>
      jsonResponse(PROBLEM, 403)
    )
    await perform()

    await waitFor(() =>
      expect(failure).toHaveBeenCalledWith(`Failed to ${verb}: ${REASON}`)
    )
  })
}

test('a failure with no reason in the body never prints "undefined"', async () => {
  const failure = setup(
    'POST /projects/1/alarms/9/acknowledge',
    () => new Response('', { status: 502 })
  )
  await click(/^Ack$/)

  await waitFor(() => expect(failure).toHaveBeenCalled())
  expect(String(failure.mock.calls[0]?.[0])).not.toContain('undefined')
})
