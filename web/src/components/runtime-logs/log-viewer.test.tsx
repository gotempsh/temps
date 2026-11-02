// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import { act, cleanup, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import LogViewer from './log-viewer'

installBrowserWindow()

/** Records every socket the viewer opens and lets a test drive it. */
class FakeWebSocket {
  static opened: FakeWebSocket[] = []
  onopen: (() => void) | null = null
  onmessage: ((event: { data: string }) => void) | null = null
  onerror: (() => void) | null = null
  onclose: ((event: { code: number }) => void) | null = null
  constructor(readonly url: string) {
    FakeWebSocket.opened.push(this)
  }
  close() {}
}

const realWebSocket = globalThis.WebSocket
beforeEach(() => {
  FakeWebSocket.opened = []
  globalThis.WebSocket = FakeWebSocket as unknown as typeof WebSocket
})
afterEach(() => {
  cleanup()
  globalThis.WebSocket = realWebSocket
})

const container = (id: string) => ({
  container_id: id,
  container_name: `example-${id}`,
  service_name: 'example',
  status: 'running',
})

function renderViewer(containers: ReturnType<typeof container>[]) {
  mockApi({
    'GET /projects/7/environments': () =>
      jsonResponse([
        { id: 3, name: 'production', slug: 'production', is_preview: false },
      ]),
    'GET /projects/7/environments/3/containers': () =>
      jsonResponse({ containers }),
  })
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter>
        <LogViewer
          project={{ id: 7, slug: 'example' } as unknown as ProjectResponse}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('all-containers streams that cannot connect show an error, not a blank pane', async () => {
  const view = renderViewer([container('a'), container('b')])

  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(2))
  expect(view.getByText('Connecting to log stream...')).toBeTruthy()

  act(() => FakeWebSocket.opened[0].onclose?.({ code: 1006 }))

  await view.findByText(/Connection lost/)
})

test('all-containers streams that opened say they are waiting for output', async () => {
  const view = renderViewer([container('a'), container('b')])

  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(2))
  act(() => FakeWebSocket.opened[0].onopen?.())

  await view.findByText(/Connected\. Waiting for these containers/)
  expect(view.queryByText('Connecting to log stream...')).toBeNull()
})

test('a stream that keeps failing gives up while the other keeps streaming, and says so', async () => {
  const view = renderViewer([container('a'), container('b')])
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(2))
  const [healthy, failing] = FakeWebSocket.opened
  act(() => healthy.onopen?.())

  // Each failure redials after a delay, up to the limit for that stream.
  act(() => failing.onclose?.({ code: 1006 }))
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(3), {
    timeout: 3000,
  })
  act(() => FakeWebSocket.opened[2].onclose?.({ code: 1006 }))
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(4), {
    timeout: 3000,
  })
  act(() => FakeWebSocket.opened[3].onclose?.({ code: 1006 }))

  await view.findByText(/Not showing logs from example-b/)
  await new Promise((resolve) => setTimeout(resolve, 2200))
  expect(FakeWebSocket.opened).toHaveLength(4)
}, 12_000)

test('when the last stream closes normally the pane stops saying it is connected', async () => {
  const view = renderViewer([container('a'), container('b')])
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(2))
  const [first, second] = FakeWebSocket.opened
  act(() => {
    first.onopen?.()
    second.onopen?.()
  })
  await view.findByText(/Connected\. Waiting for these containers/)

  act(() => first.onclose?.({ code: 1000 }))
  expect(view.queryByText(/The log stream ended/)).toBeNull()
  act(() => second.onclose?.({ code: 1000 }))

  await view.findByText(/The log stream ended/)
  expect(view.queryByText(/Connected\. Waiting/)).toBeNull()

  act(() => view.getByRole('button', { name: 'Reconnect' }).click())
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(4))
})

test('a single stream can end and be reconnected more than once', async () => {
  const view = renderViewer([container('a')])
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(1))
  act(() => FakeWebSocket.opened[0].onopen?.())
  act(() => FakeWebSocket.opened[0].onclose?.({ code: 1000 }))
  await view.findByText(/The log stream ended/)

  act(() => view.getByRole('button', { name: 'Reconnect' }).click())
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(2))
  act(() => FakeWebSocket.opened[1].onopen?.())
  await waitFor(() =>
    expect(view.queryByText(/The log stream ended/)).toBeNull()
  )

  // The reconnected socket is watched like the first: its end is noticed too.
  act(() => FakeWebSocket.opened[1].onclose?.({ code: 1000 }))
  await view.findByText(/The log stream ended/)
  act(() => view.getByRole('button', { name: 'Reconnect' }).click())
  await waitFor(() => expect(FakeWebSocket.opened).toHaveLength(3))
})

test('an environment with no containers says so instead of connecting forever', async () => {
  const view = renderViewer([])

  await view.findByText('No containers are running in this environment.')
  expect(view.queryByText('Connecting to log stream...')).toBeNull()
  expect(FakeWebSocket.opened).toHaveLength(0)
})
