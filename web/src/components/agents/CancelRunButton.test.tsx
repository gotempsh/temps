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
import { useCancelRun } from './useCancelRun'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

const CANCEL = 'POST /projects/3/agents/runs/17/cancel'

/** Wired the way the run page wires it: the page owns the mutation. */
function RunPageCancel({ onCancelled }: { onCancelled: () => void }) {
  const cancel = useCancelRun(3, 17, onCancelled)
  return (
    <CancelRunButton
      pending={cancel.isPending}
      onCancel={() => cancel.mutate()}
    />
  )
}

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
      <RunPageCancel onCancelled={() => (reloads += 1)} />
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

test('a pending cancel ignores clicks without the page having to guard them', () => {
  let cancels = 0
  const view = render(
    <CancelRunButton pending onCancel={() => (cancels += 1)} />
  )
  const button = view.getByRole('button', { name: 'Cancel' })
  fireEvent.click(button)
  expect(cancels).toBe(0)
  expect(button.getAttribute('aria-disabled')).toBe('true')
  expect((button as HTMLButtonElement).disabled).toBe(false)
})
