// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type {
  ProjectIpAccessDetail,
  UptimeMonitorIpBlock,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ProjectIpAccessCard } from './ProjectIpAccessCard'

installBrowserWindow()
afterEach(cleanup)

const PATH = '/projects/2/ip-access'

const block: UptimeMonitorIpBlock = {
  project_id: 2,
  environment_id: null,
  policy_scope: 'project',
  mode: 'allow',
  reason: 'not_allow_listed',
  monitor_addresses: ['127.0.0.1'],
  blocked_addresses: ['127.0.0.1'],
  matched_cidrs: [],
  suggested_cidrs: ['127.0.0.1/32'],
  suggested_description: 'Temps uptime monitor',
  message:
    "Blocked by this project's IP access list: Temps' uptime monitor connects from 127.0.0.1, which the list does not allow.",
}

const stored: ProjectIpAccessDetail = {
  project_id: 2,
  environment_id: null,
  configured: true,
  mode: 'allow',
  cidrs: [{ cidr: '198.51.100.7/32', description: 'Operator' }],
  max_cidrs: 256,
  revision: 4,
  updated_at: '2026-10-13T09:30:00Z',
  your_ip: '198.51.100.7',
  uptime_monitor_block: block,
}

test('the list cannot be edited while "Allow monitor" saves it', async () => {
  let finishSave: (response: Response) => void = () => {}
  const api = mockApi({
    [`GET ${PATH}`]: () => jsonResponse(stored),
    [`PUT ${PATH}`]: () =>
      new Promise<Response>((resolve) => {
        finishSave = resolve
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <ProjectIpAccessCard projectId={2} projectSlug="web" />
      </QueryClientProvider>
    </MemoryRouter>
  )

  const entry = (await view.findByDisplayValue(
    '198.51.100.7/32'
  )) as HTMLInputElement
  expect(entry.closest('fieldset')?.disabled).toBe(false)

  fireEvent.click(view.getByRole('button', { name: 'Allow monitor' }))
  // The save is in flight: an edit now would be replaced by its result.
  await waitFor(() =>
    expect(api.calls.some((call) => call.method === 'PUT')).toBe(true)
  )
  expect(entry.closest('fieldset')?.disabled).toBe(true)

  finishSave(
    jsonResponse({
      ...stored,
      revision: 5,
      cidrs: [
        ...stored.cidrs,
        { cidr: '127.0.0.1/32', description: 'Temps uptime monitor' },
      ],
      uptime_monitor_block: null,
    })
  )
  await view.findByDisplayValue('127.0.0.1/32')
  const reloaded = view.getByDisplayValue('198.51.100.7/32')
  expect(reloaded.closest('fieldset')?.disabled).toBe(false)
  api.restore()
})
