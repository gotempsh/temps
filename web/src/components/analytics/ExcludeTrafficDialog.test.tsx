// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { toast } from 'sonner'
import {
  getFilteredTrafficQueryKey,
  listTrafficFiltersQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ExcludeTrafficDialog } from './ExcludeTrafficDialog'

installBrowserWindow()
afterEach(() => {
  cleanup()
  // Sonner keeps its toasts in a module-level store shared by every test.
  toast.dismiss()
})

const TARGET = { kind: 'asn' as const, value: '64500' }

function Dialog({
  client,
  projectId,
}: {
  client: QueryClient
  projectId: number
}) {
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <ExcludeTrafficDialog
          projectId={projectId}
          projectSlug={`project-${projectId}`}
          target={TARGET}
          onClose={() => {}}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function keysFor(projectId: number) {
  const path = { path: { project_id: projectId } }
  return [listTrafficFiltersQueryKey(path), getFilteredTrafficQueryKey(path)]
}

function invalidated(client: QueryClient, projectId: number) {
  return keysFor(projectId).map(
    (queryKey) => client.getQueryState(queryKey)?.isInvalidated
  )
}

test('an exclusion confirmed before switching projects refreshes its own project', async () => {
  let finishCreate: (response: Response) => void = () => {}
  const api = mockApi({
    'POST /projects/1/analytics/traffic-filters': () =>
      new Promise<Response>((resolve) => {
        finishCreate = resolve
      }),
  })
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  // Both projects' filter list and summary are cached (not mounted here).
  for (const projectId of [1, 2]) {
    for (const queryKey of keysFor(projectId)) {
      client.setQueryData(queryKey, {})
    }
  }
  const view = render(<Dialog client={client} projectId={1} />)

  await act(async () => {
    fireEvent.click(
      await view.findByRole('button', { name: 'Exclude from analytics' })
    )
  })
  await waitFor(() =>
    expect(api.called('POST', '/projects/1/analytics/traffic-filters')).toBe(
      true
    )
  )

  // The user moves to another project while the request is in flight.
  view.rerender(<Dialog client={client} projectId={2} />)
  await act(async () => {
    finishCreate(
      jsonResponse(
        {
          id: 5,
          project_id: 1,
          kind: 'asn',
          value: '64500',
          note: null,
          created_at: '2026-01-01T00:00:00Z',
        },
        201
      )
    )
  })

  await waitFor(() => expect(invalidated(client, 1)).toEqual([true, true]))
  expect(invalidated(client, 2)).toEqual([false, false])
  api.restore()
  client.clear()
})
