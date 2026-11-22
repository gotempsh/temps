// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { NodeStateStreamStatus } from './NodeStateStreamStatus'

installBrowserWindow()
afterEach(cleanup)

const healthy = {
  configured: true,
  watermark_lag_ms: 12,
  reads: 1_204,
  full_snapshots: 3,
}

function renderStatus(respond: () => Response) {
  mockApi({ 'GET /node-state/status': respond })
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <NodeStateStreamStatus />
    </QueryClientProvider>
  )
}

test('a healthy stream is just its verdict, with the numbers one click away', async () => {
  const view = renderStatus(() => jsonResponse(healthy))
  expect(await view.findByText('Streaming')).toBeTruthy()
  expect(view.getByRole('status').textContent).not.toContain('1,204')

  fireEvent.click(view.getByRole('button', { name: 'About updates to nodes' }))
  expect(await view.findByText('1,204')).toBeTruthy()
  expect(view.getByText('12 ms')).toBeTruthy()
  expect(view.getByText('3')).toBeTruthy()
  expect(view.getByText(/desired-state stream/)).toBeTruthy()
  expect(view.getByText(/A warning shows from 5 s\./)).toBeTruthy()
})

test('a stream no node has read from does not claim to be streaming', async () => {
  const view = renderStatus(() =>
    jsonResponse({ ...healthy, reads: 0, full_snapshots: 0 })
  )
  expect(await view.findByText('Ready')).toBeTruthy()
  expect(view.queryByText('Streaming')).toBeNull()
})

test('lag says what is wrong without opening anything', async () => {
  const view = renderStatus(() =>
    jsonResponse({ ...healthy, watermark_lag_ms: 12_000 })
  )
  expect(await view.findByText('Lagging 12 s')).toBeTruthy()
  expect(view.getByRole('status').textContent).toContain(
    'an old open transaction'
  )
})

test('a stream that is not running says why, and offers no numbers', async () => {
  const view = renderStatus(() =>
    jsonResponse({
      configured: false,
      reason: 'the workload plane is disabled',
      watermark_lag_ms: 0,
      reads: 0,
      full_snapshots: 0,
    })
  )
  expect(await view.findByText('Not running')).toBeTruthy()
  expect(view.getByRole('status').textContent).toContain(
    'the workload plane is disabled'
  )
  fireEvent.click(view.getByRole('button', { name: 'About updates to nodes' }))
  expect(await view.findByText(/desired-state stream/)).toBeTruthy()
  expect(view.queryByText('Requests')).toBeNull()
})

test('a refused read explains itself and still offers the explanation', async () => {
  const view = renderStatus(() =>
    jsonResponse({ status: 403, title: 'Forbidden', detail: 'Forbidden' }, 403)
  )
  expect(
    await view.findByText(
      'You do not have permission to read the stream status.'
    )
  ).toBeTruthy()
  expect(
    view.getByRole('button', { name: 'About updates to nodes' })
  ).toBeTruthy()
})
