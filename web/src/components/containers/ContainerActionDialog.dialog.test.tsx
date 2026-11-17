// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { client } from '@/api/client/client.gen'
import { installBrowserWindow } from '@/test/dom'
import { ContainerActionDialog } from './ContainerActionDialog'

installBrowserWindow()

const realFetch = globalThis.fetch
const realBaseUrl = client.getConfig().baseUrl
afterEach(() => {
  globalThis.fetch = realFetch
  client.setConfig({ baseUrl: realBaseUrl })
  cleanup()
})

/**
 * Answer every request with `status` and `body`, as the API would, and
 * record the URLs asked for. The client's relative base URL is made absolute
 * so a `Request` can be built outside a browser.
 */
function answerWith(status: number, body: unknown): string[] {
  const requested: string[] = []
  client.setConfig({ baseUrl: 'http://console.example.test/api' })
  globalThis.fetch = (async (request: Request) => {
    requested.push(request.url)
    return new Response(JSON.stringify(body), {
      status,
      headers: {
        'Content-Type':
          status >= 400 ? 'application/problem+json' : 'application/json',
      },
    })
  }) as unknown as typeof fetch
  return requested
}

async function confirmRestart(onClose: () => void) {
  const queryClient = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  render(
    <QueryClientProvider client={queryClient}>
      <ContainerActionDialog
        projectId="3"
        environmentId="5"
        action="restart"
        containerId="vm-1"
        onClose={onClose}
      />
    </QueryClientProvider>
  )
  const confirm = Array.from(
    document.body.querySelectorAll('[role="alertdialog"] button')
  ).find((button) => button.textContent === 'Confirm')
  expect(confirm).toBeDefined()
  await act(async () => {
    fireEvent.click(confirm!)
    // Let the request and the mutation callbacks settle.
    await new Promise((resolve) => setTimeout(resolve, 20))
  })
}

const RESTART_URL =
  'http://console.example.test/api/projects/3/environments/5/containers/vm-1/restart'

test('a refused restart keeps the dialog open', async () => {
  const requested = answerWith(400, {
    title: 'Invalid Deployment State',
    status: 400,
    detail:
      'Cannot restart container vm-1: it runs in a Firecracker microVM (workload 7)',
  })
  const onClose = mock(() => {})

  await confirmRestart(onClose)

  expect(requested).toEqual([RESTART_URL])
  expect(onClose).not.toHaveBeenCalled()
  expect(document.body.querySelector('[role="alertdialog"]')).not.toBeNull()
})

test('a restart that succeeds closes the dialog', async () => {
  const requested = answerWith(200, {})
  const onClose = mock(() => {})

  await confirmRestart(onClose)

  expect(requested).toEqual([RESTART_URL])
  expect(onClose).toHaveBeenCalledTimes(1)
})
