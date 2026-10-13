// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import type { ReactElement } from 'react'
import { MemoryRouter } from 'react-router'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { installBrowserWindow } from '@/test/dom'
import { CreateProjectGroupDialog } from './CreateProjectGroupDialog'
import type { PickedService } from './ServicePicker'

installBrowserWindow()
afterEach(cleanup)

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

function dialog(open: boolean, initialServices?: PickedService[]) {
  return wrap(
    <CreateProjectGroupDialog
      open={open}
      onOpenChange={() => {}}
      initialServices={initialServices}
    />
  )
}

function nameInput(): HTMLInputElement | null {
  return document.body.querySelector<HTMLInputElement>('#group-name')
}

test('reopening the dialog starts from an empty form, not the last draft', async () => {
  const api = mockApi({
    'GET /projects': () => jsonResponse({ projects: [], total: 0 }),
  })
  const view = render(dialog(true))
  expect(nameInput()?.value).toBe('')

  await act(async () => {
    fireEvent.change(nameInput()!, { target: { value: 'Acme Shop' } })
  })
  expect(nameInput()?.value).toBe('Acme Shop')

  await act(async () => {
    view.rerender(dialog(false))
  })
  await act(async () => {
    view.rerender(dialog(true))
  })
  expect(nameInput()?.value).toBe('')
  api.restore()
})
