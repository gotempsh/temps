// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { toast } from 'sonner'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { MonitoringCard } from './MonitoringCard'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const REASON = 'Missing permission ExternalServicesWrite'
const refused = () => jsonResponse({ title: 'Forbidden', detail: REASON }, 403)

const RULE = {
  id: 5,
  name: 'High connections',
  metric_name: 'postgres_connections_active',
  comparator: 'gt',
  threshold: 10,
  severity: 'warning',
  enabled: true,
  service_id: 3,
  created_at: '2026-11-01T10:00:00Z',
  updated_at: '2026-11-01T10:00:00Z',
}

function setup(
  metricsEnabled: boolean,
  routes: Parameters<typeof mockApi>[0] = {}
) {
  const failure = spyOn(toast, 'error').mockImplementation(() => 'id')
  mockApi({
    'GET /external-services/3/metrics/latest': () =>
      jsonResponse({ postgres_connections_active: 4 }),
    'GET /external-services/3/metrics/status': () =>
      jsonResponse({ last_received_at: null }),
    'GET /external-services/3/metrics': () => jsonResponse([]),
    'GET /external-services/3/metrics/by-database': () => jsonResponse([]),
    'GET /external-services/3/metrics/alert-rules': () => jsonResponse([RULE]),
    ...routes,
  })
  render(
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
        <MonitoringCard
          serviceId={3}
          engine="postgres"
          metricsEnabled={metricsEnabled}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  return failure
}

async function click(name: string | RegExp) {
  const target = await screen.findByRole('button', { name })
  await act(async () => {
    fireEvent.click(target)
  })
}

test('a refused enable gives the reason', async () => {
  const failure = setup(false, {
    'PATCH /external-services/3/metrics/enable': refused,
  })
  await click('Enable monitoring')

  await waitFor(() =>
    expect(failure).toHaveBeenCalledWith('Failed to enable monitoring', {
      description: REASON,
    })
  )
})

test('a refused disable gives the reason', async () => {
  const failure = setup(true, {
    'PATCH /external-services/3/metrics/enable': refused,
  })
  await click('Disable')

  await waitFor(() =>
    expect(failure).toHaveBeenCalledWith('Failed to disable monitoring', {
      description: REASON,
    })
  )
})

test('a refused new alert rule gives the reason', async () => {
  const failure = setup(true, {
    'POST /external-services/3/metrics/alert-rules': refused,
  })
  await click('Alert rules')
  await click('Add rule')
  const name = await screen.findByPlaceholderText('e.g. High connection count')
  await act(async () => {
    fireEvent.change(name, { target: { value: 'Too many' } })
  })
  const dialog = await screen.findByRole('dialog')
  const submit = Array.from(dialog.querySelectorAll('button')).find(
    (b) => b.textContent === 'Add rule'
  ) as HTMLButtonElement
  await act(async () => {
    fireEvent.click(submit)
  })

  await waitFor(() =>
    expect(failure).toHaveBeenCalledWith('Failed to create alert rule', {
      description: REASON,
    })
  )
})

test('a refused threshold edit gives the reason', async () => {
  const failure = setup(true, {
    'PUT /external-services/3/metrics/alert-rules/5': refused,
  })
  await click('Alert rules')
  await click(`${RULE.comparator} ${RULE.threshold}`)
  const input = await screen.findByRole('spinbutton', {
    name: `Threshold for ${RULE.name}`,
  })
  await act(async () => {
    fireEvent.change(input, { target: { value: '20' } })
  })
  await act(async () => {
    fireEvent.blur(input)
  })

  await waitFor(() =>
    expect(failure).toHaveBeenCalledWith('Failed to update rule', {
      description: REASON,
    })
  )
})

test('a refused rule delete gives the reason', async () => {
  const failure = setup(true, {
    'DELETE /external-services/3/metrics/alert-rules/5': refused,
  })
  await click('Alert rules')
  await click(`Delete rule ${RULE.name}`)
  const dialog = await screen.findByRole('dialog')
  const confirm = Array.from(dialog.querySelectorAll('button')).find((b) =>
    b.textContent?.includes('Delete')
  ) as HTMLButtonElement
  await act(async () => {
    fireEvent.click(confirm)
  })

  await waitFor(() =>
    expect(failure).toHaveBeenCalledWith('Failed to delete rule', {
      description: REASON,
    })
  )
})
