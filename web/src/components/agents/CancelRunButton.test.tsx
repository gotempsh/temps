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
import { CancelRunButton } from './CancelRunButton'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const CANCEL = 'POST /projects/3/agents/runs/17/cancel'

test('a pending cancel keeps the button focused and ignores further clicks', async () => {
  let release: (response: Response) => void = () => {}
  let reloads = 0
  const api = mockApi({
    [CANCEL]: () =>
      new Promise<Response>((resolve) => {
        release = resolve
      }),
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const view = render(
    <QueryClientProvider client={client}>
      <CancelRunButton
        projectId={3}
        runId={17}
        onCancelled={() => (reloads += 1)}
      />
      <Toaster />
    </QueryClientProvider>
  )
  const button = view.getByRole('button', { name: 'Cancel' })
  button.focus()

  await act(async () => {
    fireEvent.click(button)
  })
  await waitFor(() => expect(button.getAttribute('aria-disabled')).toBe('true'))

  // A native `disabled` would have dropped focus to the page.
  expect((button as HTMLButtonElement).disabled).toBe(false)
  expect(document.activeElement).toBe(button)

  await act(async () => {
    fireEvent.click(button)
  })
  // A second click while pending sends no second cancel.
  expect(api.calls.filter((call) => call.method === 'POST').length).toBe(1)

  await act(async () => {
    release(jsonResponse({}))
  })
  await waitFor(() => expect(reloads).toBe(1))
  api.restore()
})
