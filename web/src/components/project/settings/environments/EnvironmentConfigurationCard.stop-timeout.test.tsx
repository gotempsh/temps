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
import { Toaster, toast } from 'sonner'
import type { EnvironmentResponse, ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { EnvironmentConfigurationCard } from './EnvironmentConfigurationCard'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
  restoreAllMockApis()
})

const SETTINGS = 'PUT /projects/3/environments/7/settings'

function renderCard(stopTimeoutSeconds: number | null) {
  const sent: Array<Record<string, unknown>> = []
  const environment = {
    id: 7,
    project_id: 3,
    name: 'production',
    placement_warning: null,
    deployment_config: { replicas: 1, stopTimeoutSeconds },
  } as unknown as EnvironmentResponse
  mockApi({
    'GET /internal/nodes': () => jsonResponse({ nodes: [] }),
    [SETTINGS]: async (request) => {
      sent.push((await request.json()) as Record<string, unknown>)
      return jsonResponse(environment)
    },
  })
  const project = { id: 3, slug: 'app' } as unknown as ProjectResponse
  const view = render(
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
      <MemoryRouter>
        <EnvironmentConfigurationCard
          project={project}
          environment={environment}
          onUpdate={() => {}}
        />
        <Toaster />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return { view, sent }
}

async function setStopTimeoutAndSave(
  view: ReturnType<typeof render>,
  value: string
) {
  await act(async () => {
    fireEvent.change(view.getByLabelText('Seconds'), { target: { value } })
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save changes' }))
  })
}

test('the environment stop timeout override is saved', async () => {
  const { view, sent } = renderCard(null)
  await setStopTimeoutAndSave(view, '120')
  await waitFor(() => expect(sent).toHaveLength(1))
  expect(sent[0].stop_timeout_seconds).toBe(120)
})

test('clearing the override sends null so the environment follows the project', async () => {
  const { view, sent } = renderCard(90)
  expect((view.getByLabelText('Seconds') as HTMLInputElement).value).toBe('90')
  await setStopTimeoutAndSave(view, '')
  await waitFor(() => expect(sent).toHaveLength(1))
  expect(sent[0].stop_timeout_seconds).toBeNull()
})

test('an out-of-range stop timeout is refused before anything is sent', async () => {
  const { view, sent } = renderCard(null)
  const input = view.getByLabelText('Seconds') as HTMLInputElement
  await act(async () => {
    fireEvent.change(input, { target: { value: '7200' } })
  })
  // The browser's own `max` check stops the Save button first; submit the
  // form directly so the console's check (which also covers a browser that
  // skips constraint validation) is what refuses it.
  await act(async () => {
    fireEvent.submit(input.form!)
  })
  expect(
    await view.findAllByText(
      'Enter a whole number of seconds between 1 and 3600'
    )
  ).not.toHaveLength(0)
  expect(sent).toHaveLength(0)
})
