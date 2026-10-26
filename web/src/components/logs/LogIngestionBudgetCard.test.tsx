// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  LogIngestionBudgetCard,
  type LogIngestionBudget,
} from './LogIngestionBudgetCard'

installBrowserWindow()
afterEach(cleanup)

const platformBudget = {
  lines_per_sec: 250,
  bytes_per_sec: 256 * 1024,
  burst_seconds: 10,
  setup_path: '/settings/metrics-monitoring#container-log-ingest-budget',
  throttled: [],
}

function setup(
  value: { lines?: number | null; kib?: number | null } | undefined,
  onSave: (budget: LogIngestionBudget) => Promise<unknown> = () =>
    Promise.resolve()
) {
  const api = mockApi({
    'GET /logs/ingest-budget': () => jsonResponse(platformBudget),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const ui = (next: typeof value, isSaving = false) => (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <LogIngestionBudgetCard
          scope="project"
          value={next}
          onSave={onSave}
          isSaving={isSaving}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  const view = render(ui(value))
  const lines = () =>
    view.getByLabelText('Lines per second') as HTMLInputElement
  const kib = () => view.getByLabelText('KiB per second') as HTMLInputElement
  return { api, view, ui, lines, kib }
}

// A refetch of the saved budget (the parent's query refreshing) must not
// overwrite what the user is typing; untouched fields still follow the server.
test('a refresh of the saved budget keeps the field being edited', () => {
  const { api, view, ui, lines, kib } = setup({ lines: 100, kib: null })
  expect(lines().value).toBe('100')
  expect(kib().value).toBe('')

  fireEvent.change(lines(), { target: { value: '500' } })
  view.rerender(ui({ lines: 200, kib: 64 }))

  expect(lines().value).toBe('500')
  expect(kib().value).toBe('64')
  api.restore()
})

test('saves blank as null (platform budget) and numbers as numbers', async () => {
  const onSave = mock((_budget: LogIngestionBudget) => Promise.resolve())
  const { api, view, lines } = setup({ lines: 100, kib: 32 }, onSave)

  fireEvent.change(lines(), { target: { value: '' } })
  fireEvent.click(view.getByRole('button', { name: 'Save budget' }))

  await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1))
  expect(onSave.mock.calls[0]![0]).toEqual({ lines: null, kib: 32 })
  api.restore()
})

test('an out-of-range value is listed and not saved', async () => {
  const onSave = mock((_budget: LogIngestionBudget) => Promise.resolve())
  const { api, view, lines } = setup({ lines: null, kib: null }, onSave)

  fireEvent.change(lines(), { target: { value: '-3' } })
  fireEvent.click(view.getByRole('button', { name: 'Save budget' }))

  await view.findByText('Fix 1 field before saving')
  expect(onSave).not.toHaveBeenCalled()
  api.restore()
})

test('a failed save does not escape as an unhandled rejection', async () => {
  const onSave = mock((_budget: LogIngestionBudget) =>
    Promise.reject(new Error('500 from the server'))
  )
  const { api, view } = setup({ lines: 10, kib: 10 }, onSave)

  fireEvent.click(view.getByRole('button', { name: 'Save budget' }))

  await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1))
  // The form settles back to submittable instead of throwing.
  await waitFor(() =>
    expect(
      view
        .getByRole('button', { name: 'Save budget' })
        .getAttribute('aria-busy')
    ).not.toBe('true')
  )
  api.restore()
})

test('fields are read-only while a save is in flight', () => {
  const { api, view, ui, lines, kib } = setup({ lines: 10, kib: 10 })
  view.rerender(ui({ lines: 10, kib: 10 }, true))
  expect(lines().readOnly).toBe(true)
  expect(kib().readOnly).toBe(true)
  api.restore()
})
