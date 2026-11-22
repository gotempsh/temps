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
import { Toaster, toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { useCancelRun } from './useCancelRun'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const CANCEL = 'POST /projects/3/agents/runs/17/cancel'

function CancelButton({ onCancelled }: { onCancelled: () => void }) {
  const cancel = useCancelRun(3, 17, onCancelled)
  return (
    <button type="button" onClick={() => cancel.mutate()}>
      Cancel
    </button>
  )
}

function renderButton(onCancelled: () => void) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <CancelButton onCancelled={onCancelled} />
      <Toaster />
    </QueryClientProvider>
  )
}

test('a refused cancel says why and does not reload the run', async () => {
  let reloads = 0
  const api = mockApi({
    [CANCEL]: () =>
      jsonResponse(
        {
          title: 'Conflict',
          status: 409,
          detail: 'Run 17 already finished and cannot be cancelled',
        },
        409
      ),
  })
  const view = renderButton(() => (reloads += 1))
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Cancel' }))
  })

  expect(await view.findByText('Failed to cancel run')).toBeTruthy()
  expect(
    await view.findByText('Run 17 already finished and cannot be cancelled')
  ).toBeTruthy()
  expect(reloads).toBe(0)
  api.restore()
})

test('an accepted cancel reloads the run', async () => {
  let reloads = 0
  const api = mockApi({ [CANCEL]: () => jsonResponse({}) })
  const view = renderButton(() => (reloads += 1))
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Cancel' }))
  })

  await waitFor(() => expect(reloads).toBe(1))
  expect(view.queryByText('Failed to cancel run')).toBeNull()
  api.restore()
})
