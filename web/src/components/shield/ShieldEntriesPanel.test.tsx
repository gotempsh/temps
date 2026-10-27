// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  ShieldEntriesPanel,
  type ShieldEntriesScope,
} from './ShieldEntriesPanel'

installBrowserWindow()
afterEach(cleanup)

const PLATFORM_PATH = '/settings/shield/entries'
const PROJECT_PATH = '/projects/3/shield/entries'

const ENTRY = {
  id: 5,
  scope: 'platform',
  project_id: null,
  network: '198.51.100.0/24',
  ja4: null,
  verdict: 'block',
  note: null,
  expires_at: null,
  created_by: 1,
  created_at: '2026-10-27T10:00:00Z',
}

function renderPanel(
  scope: ShieldEntriesScope,
  wrap?: (panel: ReactNode) => ReactNode
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const panel = <ShieldEntriesPanel scope={scope} />
  return render(
    <QueryClientProvider client={client}>
      {wrap ? wrap(panel) : panel}
    </QueryClientProvider>
  )
}

/** The dialog's submit button: the last "Add entry" on the page. */
function submitButton(view: ReturnType<typeof renderPanel>) {
  const buttons = view.getAllByRole('button', { name: 'Add entry' })
  return buttons[buttons.length - 1]
}

test('a platform entry is added through the platform endpoint', async () => {
  let added = false
  let sentBody: unknown = null
  const api = mockApi({
    [`GET ${PLATFORM_PATH}`]: () => jsonResponse(added ? [ENTRY] : []),
    [`POST ${PLATFORM_PATH}`]: async (request) => {
      sentBody = await request.json()
      added = true
      return jsonResponse(ENTRY, 201)
    },
  })
  const view = renderPanel({ kind: 'platform' })

  await view.findByText('No operator entries yet')
  expect(view.getByText(/every service on this platform/)).toBeTruthy()
  fireEvent.click(view.getByRole('button', { name: 'Add entry' }))
  const value = await view.findByLabelText('Address or CIDR')
  fireEvent.change(value, { target: { value: '198.51.100.0/24' } })
  fireEvent.click(submitButton(view))

  await view.findByText('198.51.100.0/24')
  expect(sentBody).toEqual({
    network: '198.51.100.0/24',
    ja4: null,
    verdict: 'block',
    note: null,
    expires_at: null,
  })
  expect(api.called('GET', PROJECT_PATH)).toBe(false)
  api.restore()
})

test('adding an entry does not submit the settings form it sits in', async () => {
  let outerSubmits = 0
  const api = mockApi({
    [`GET ${PROJECT_PATH}`]: () => jsonResponse([]),
    [`POST ${PROJECT_PATH}`]: () =>
      jsonResponse({ ...ENTRY, scope: 'project', project_id: 3 }, 201),
  })
  const view = renderPanel({ kind: 'project', projectId: 3 }, (panel) => (
    <form
      onSubmit={(event) => {
        event.preventDefault()
        outerSubmits += 1
      }}
    >
      {panel}
    </form>
  ))

  await view.findByText('No operator entries yet')
  expect(view.getByText(/every environment of this service/)).toBeTruthy()
  fireEvent.click(view.getByRole('button', { name: 'Add entry' }))
  const value = await view.findByLabelText('Address or CIDR')
  fireEvent.change(value, { target: { value: '198.51.100.0/24' } })
  fireEvent.click(submitButton(view))

  await waitFor(() => expect(api.called('POST', PROJECT_PATH)).toBe(true))
  await waitFor(() =>
    expect(view.queryByLabelText('Address or CIDR')).toBeNull()
  )
  expect(api.called('GET', PLATFORM_PATH)).toBe(false)
  expect(outerSubmits).toBe(0)
  api.restore()
})
