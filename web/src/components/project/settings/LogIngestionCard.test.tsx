// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { LogIngestionCard } from './LogIngestionCard'

installBrowserWindow()
afterEach(cleanup)

// The refetch has to wait for the save: refetching while the request is in
// flight reloads the old budget, and the card shows it as if the save failed.
test('refetches the project only after the save has landed', async () => {
  let respond: (response: Response) => void = () => {}
  const pending = new Promise<Response>((resolve) => {
    respond = resolve
  })
  const api = mockApi({
    'GET /logs/ingest-budget': () =>
      jsonResponse({
        lines_per_sec: 250,
        bytes_per_sec: 256 * 1024,
        burst_seconds: 10,
        setup_path: '/settings/metrics-monitoring',
        throttled: [],
      }),
    'POST /projects/7/settings': () => pending,
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const refetch = mock(() => {})
  const project = {
    id: 7,
    log_ingest_lines_per_sec: null,
    log_ingest_kib_per_sec: null,
  } as unknown as ProjectResponse

  const view = render(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <LogIngestionCard project={project} refetch={refetch} />
      </MemoryRouter>
    </QueryClientProvider>
  )
  fireEvent.change(view.getByLabelText('Lines per second'), {
    target: { value: '500' },
  })
  fireEvent.click(view.getByRole('button', { name: 'Save' }))

  await waitFor(() =>
    expect(api.called('POST', '/projects/7/settings')).toBe(true)
  )
  // Give any premature refetch a chance to run.
  await new Promise((resolve) => setTimeout(resolve, 50))
  expect(refetch).not.toHaveBeenCalled()

  respond(jsonResponse({ ...project, log_ingest_lines_per_sec: 500 }))
  await waitFor(() => expect(refetch).toHaveBeenCalledTimes(1))
  api.restore()
})
