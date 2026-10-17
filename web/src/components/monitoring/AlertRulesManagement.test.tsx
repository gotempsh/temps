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
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { AlertRulesManagement } from './AlertRulesManagement'
import { installBrowserWindow } from '@/test/dom'

const RULE = {
  id: 11,
  project_id: 7,
  name: 'Checkout failures',
  enabled: true,
  trigger_type: 'new_issue',
  trigger_config: {},
  cooldown_minutes: 15,
  notification_priority: 'Normal',
  created_at: '2026-10-12T00:00:00Z',
  updated_at: '2026-10-12T00:00:00Z',
}

installBrowserWindow()

const unhandled: unknown[] = []
const onUnhandled = (reason: unknown) => unhandled.push(reason)
process.on('unhandledRejection', onUnhandled)
afterEach(() => {
  cleanup()
  unhandled.length = 0
})

async function openDeleteDialog(view: ReturnType<typeof render>) {
  const trigger = await view.findByLabelText(`Actions for ${RULE.name}`)
  await act(async () => {
    fireEvent.pointerDown(trigger, { button: 0, ctrlKey: false })
  })
  const item = await view.findByText('Delete')
  await act(async () => {
    fireEvent.click(item)
  })
}

function renderRules() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AlertRulesManagement projectId={7} />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('Delete asks for confirmation naming the rule and sends nothing until confirmed', async () => {
  const api = mockApi({
    'GET /projects/7/error-alert-rules': () => jsonResponse([RULE]),
    'DELETE /projects/7/error-alert-rules/11': () => jsonResponse({}, 200),
  })
  const view = renderRules()
  await openDeleteDialog(view)

  expect(await view.findByText(`Delete alert rule ${RULE.name}?`)).toBeTruthy()
  expect(api.called('DELETE', '/projects/7/error-alert-rules/11')).toBe(false)

  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Cancel' }))
  })
  expect(api.called('DELETE', '/projects/7/error-alert-rules/11')).toBe(false)

  await openDeleteDialog(view)
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  await waitFor(() =>
    expect(api.called('DELETE', '/projects/7/error-alert-rules/11')).toBe(true)
  )
  view.unmount()
  api.restore()
})

test('a failed delete does not raise an unhandled promise rejection', async () => {
  const api = mockApi({
    'GET /projects/7/error-alert-rules': () => jsonResponse([RULE]),
    'DELETE /projects/7/error-alert-rules/11': () =>
      jsonResponse({ title: 'Internal Server Error', detail: 'boom' }, 500),
  })
  const view = renderRules()
  await openDeleteDialog(view)
  await act(async () => {
    fireEvent.click(await view.findByRole('button', { name: 'Delete' }))
  })
  await waitFor(() =>
    expect(api.called('DELETE', '/projects/7/error-alert-rules/11')).toBe(true)
  )
  // Give any stray rejection a chance to surface.
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50))
  })
  expect(unhandled).toHaveLength(0)
  view.unmount()
  api.restore()
})
