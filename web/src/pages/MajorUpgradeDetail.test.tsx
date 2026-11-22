// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes } from 'react-router'
import { act, cleanup, fireEvent, render } from '@testing-library/react'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { shouldRetryQuery } from '@/lib/query-retry'
import { installBrowserWindow } from '@/test/dom'
import { MajorUpgradeDetail } from './MajorUpgradeDetail'

installBrowserWindow()

const originalFetch = globalThis.fetch
afterEach(() => {
  cleanup()
  globalThis.fetch = originalFetch
})

function respondWith(status: number, body: unknown) {
  const requested: string[] = []
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    requested.push(String(input))
    return new Response(JSON.stringify(body), {
      status,
      headers: { 'Content-Type': 'application/json' },
    })
  }) as typeof fetch
  return requested
}

function renderAt(
  path: string,
  retry: typeof shouldRetryQuery | false = shouldRetryQuery
) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry } },
  })
  return render(
    <QueryClientProvider client={client}>
      <BreadcrumbProvider>
        <MemoryRouter initialEntries={[path]}>
          <Routes>
            <Route
              path="/storage/:id/upgrades/:upgradeId"
              element={<MajorUpgradeDetail />}
            />
          </Routes>
        </MemoryRouter>
      </BreadcrumbProvider>
    </QueryClientProvider>
  )
}

test('an unknown upgrade id shows a not-found state instead of loading forever', async () => {
  respondWith(404, { title: 'Not Found', detail: 'Upgrade 77 not found' })
  const view = renderAt('/storage/3/upgrades/77')
  expect(await view.findByText('Upgrade not found')).toBeTruthy()
  expect(
    view.getByText(/Upgrade #77 does not exist for service 3/)
  ).toBeTruthy()
})

test('a non-numeric id is reported as invalid without any request', async () => {
  const requested = respondWith(200, {})
  const view = renderAt('/storage/3/upgrades/abc')
  expect(await view.findByText('Invalid upgrade')).toBeTruthy()
  expect(requested).toHaveLength(0)
})

test('Retry also re-reads the logs when both reads failed and the upgrade has since finished', async () => {
  let healthy = false
  globalThis.fetch = (async (input: RequestInfo | URL) => {
    const url = String(input)
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), {
        status,
        headers: { 'Content-Type': 'application/json' },
      })
    if (!healthy) return json({ title: 'Service Unavailable' }, 503)
    if (url.endsWith('/logs')) {
      return json({ content: 'migration finished ok' })
    }
    return json({
      id: 5,
      service_id: 3,
      from_version: '16',
      to_version: '17',
      from_image: 'postgres:16',
      to_image: 'postgres:17',
      status: 'completed',
      phase: 'done',
      log_id: 'log-5',
      pre_upgrade_backup_id: null,
      rollback_volume_name: null,
      error_message: null,
      attempt: 1,
      started_at: '2026-10-12T00:00:00Z',
      finished_at: '2026-10-12T00:05:00Z',
      created_at: '2026-10-12T00:00:00Z',
    })
  }) as typeof fetch
  const view = renderAt('/storage/3/upgrades/5', false)
  const retry = await view.findByRole('button', { name: 'Retry' })
  healthy = true
  await act(async () => {
    fireEvent.click(retry)
  })
  expect(await view.findByText('migration finished ok')).toBeTruthy()
})
