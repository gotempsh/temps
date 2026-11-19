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
  within,
} from '@testing-library/react'
import { Toaster, toast } from 'sonner'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { DeployDefaultsCard } from './DeployDefaultsCard'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
  restoreAllMockApis()
})

function renderCard(stopTimeoutSeconds: number | null) {
  const sent: Array<Record<string, unknown>> = []
  const project = {
    id: 3,
    slug: 'app',
    deployment_config: { stopTimeoutSeconds },
  } as unknown as ProjectResponse
  mockApi({
    'PATCH /projects/3/deployment-config': async (request) => {
      sent.push((await request.json()) as Record<string, unknown>)
      return jsonResponse(project)
    },
  })
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
      <DeployDefaultsCard project={project} refetch={() => {}} />
      <Toaster />
    </QueryClientProvider>
  )
  return { view, sent }
}

async function saveStopTimeout(view: ReturnType<typeof render>, value: string) {
  const input = view.getByLabelText('Stop timeout (seconds)')
  await act(async () => {
    fireEvent.change(input, { target: { value } })
  })
  const form = input.closest('form') as HTMLFormElement
  await act(async () => {
    fireEvent.click(within(form).getByRole('button', { name: 'Save' }))
  })
}

test('the stop timeout sits next to the startup timeout and is saved', async () => {
  const { view, sent } = renderCard(30)
  expect(
    (view.getByLabelText('Stop timeout (seconds)') as HTMLInputElement).value
  ).toBe('30')
  expect(view.getByLabelText('Startup timeout (seconds)')).toBeTruthy()

  await saveStopTimeout(view, '120')
  await waitFor(() => expect(sent).toHaveLength(1))
  expect(sent[0].stopTimeoutSeconds).toBe(120)
  expect(await view.findByText('Port and health check saved')).toBeTruthy()
})

test('blanking a stored stop timeout resets it to the 10 second default', async () => {
  const { view, sent } = renderCard(30)
  await saveStopTimeout(view, '')
  await waitFor(() => expect(sent).toHaveLength(1))
  expect(sent[0].stopTimeoutSeconds).toBe(10)
})

test('saving the port does not pin a stop timeout the project never set', async () => {
  const { view, sent } = renderCard(null)
  const port = view.getByLabelText('Port')
  await act(async () => {
    fireEvent.change(port, { target: { value: '8080' } })
  })
  await act(async () => {
    fireEvent.click(
      within(port.closest('form') as HTMLFormElement).getByRole('button', {
        name: 'Save',
      })
    )
  })
  await waitFor(() => expect(sent).toHaveLength(1))
  expect(sent[0].exposedPort).toBe(8080)
  expect('stopTimeoutSeconds' in sent[0]).toBe(false)
})

test('an out-of-range stop timeout is refused before anything is sent', async () => {
  const { view, sent } = renderCard(null)
  const input = view.getByLabelText('Stop timeout (seconds)')
  await act(async () => {
    fireEvent.change(input, { target: { value: '0' } })
  })
  // Skip the browser's own `min` check so the form's validation answers.
  await act(async () => {
    fireEvent.submit(input.closest('form')!)
  })
  expect(
    await view.findByText('Enter a whole number of seconds between 1 and 3600')
  ).toBeTruthy()
  expect(sent).toHaveLength(0)
})
