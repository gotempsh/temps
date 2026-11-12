// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, mock, test } from 'bun:test'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type {
  HealthcheckOverrides,
  HealthcheckTimings,
  ServiceHealthcheck,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { ServiceHealthcheckView } from './ServiceHealthcheckCard'

installBrowserWindow()
afterEach(cleanup)

const BUILTIN: HealthcheckTimings = {
  interval_secs: 30,
  start_interval_secs: 2,
  start_period_secs: 90,
  timeout_secs: 5,
  retries: 3,
}

const BUILTIN_SOURCES = {
  interval_secs: 'builtin',
  start_interval_secs: 'builtin',
  start_period_secs: 'builtin',
  timeout_secs: 'builtin',
  retries: 'builtin',
} as const

const NO_OVERRIDES: HealthcheckOverrides = {
  interval_secs: null,
  start_interval_secs: null,
  start_period_secs: null,
  timeout_secs: null,
  retries: null,
}

const STANDALONE: ServiceHealthcheck = {
  service_id: 7,
  service_name: 'orders-cache',
  service_type: 'redis',
  engine: 'redis',
  capability: { configurable: true, can_apply_now: true },
  overrides: { ...NO_OVERRIDES, interval_secs: 60 },
  effective: {
    timings: { ...BUILTIN, interval_secs: 60 },
    sources: { ...BUILTIN_SOURCES, interval_secs: 'service' },
  },
  inherited: { timings: BUILTIN, sources: BUILTIN_SOURCES },
  // Created before the override: still checked every second.
  running: { ...BUILTIN, interval_secs: 1, start_interval_secs: 1 },
  pending_recreate: true,
  start_interval_supported: true,
  readiness_wait_secs: 90,
}

function renderView(
  healthcheck: ServiceHealthcheck,
  handlers: {
    onSave?: (overrides: HealthcheckOverrides) => void
    onApply?: () => void
  } = {}
) {
  return render(
    <MemoryRouter>
      <ServiceHealthcheckView
        healthcheck={healthcheck}
        onSave={handlers.onSave ?? (() => {})}
        onApply={handlers.onApply ?? (() => {})}
      />
    </MemoryRouter>
  )
}

test('shows each effective value with its source and what the running container has', () => {
  const view = renderView(STANDALONE)
  expect(view.getByText('Pending recreate')).toBeTruthy()
  expect(view.getByText(/checked every 1s \(60 per minute\)/)).toBeTruthy()
  expect(view.getByText('This service')).toBeTruthy()
  expect(view.getAllByText('Built-in').length).toBe(4)
  // Interval and start interval both differ from what a recreate gives.
  expect(view.getAllByText('Running: 1s', { selector: 'dd' }).length).toBe(2)
  expect(
    view.getByText('Healthcheck changes apply when the container is recreated.')
  ).toBeTruthy()
})

test('saving sends the typed values, with empty fields as inherit', async () => {
  const onSave = mock((_: HealthcheckOverrides) => {})
  const view = renderView(STANDALONE, { onSave })
  const interval = view.getByLabelText('Interval (s)') as HTMLInputElement
  expect(interval.value).toBe('60')
  expect(
    (view.getByLabelText('Timeout (s)') as HTMLInputElement).placeholder
  ).toBe('5')

  fireEvent.change(interval, { target: { value: '120' } })
  fireEvent.change(view.getByLabelText('Retries'), { target: { value: '4' } })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save overrides' }))
  })
  await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1))
  expect(onSave.mock.calls[0][0]).toEqual({
    ...NO_OVERRIDES,
    interval_secs: 120,
    retries: 4,
  })
})

test('an invalid value is explained and not sent', async () => {
  const onSave = mock((_: HealthcheckOverrides) => {})
  const view = renderView(STANDALONE, { onSave })
  fireEvent.change(view.getByLabelText('Interval (s)'), {
    target: { value: '5' },
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save overrides' }))
  })
  // 5s is in range, but not longer than the inherited 5s timeout.
  expect(
    await view.findByText('Timeout (5s) must be shorter than the interval (5s)')
  ).toBeTruthy()
  fireEvent.change(view.getByLabelText('Interval (s)'), {
    target: { value: '2' },
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save overrides' }))
  })
  expect(
    await view.findByText('Interval must be between 5 and 3600 seconds')
  ).toBeTruthy()
  expect(onSave).not.toHaveBeenCalled()
})

test('Apply now recreates only after the restart is confirmed', async () => {
  const onApply = mock(() => {})
  const view = renderView(STANDALONE, { onApply })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Apply now' }))
  })
  const dialog = await view.findByRole('alertdialog')
  expect(dialog.textContent).toContain('Recreate orders-cache now?')
  expect(dialog.textContent).toContain('restarts briefly')
  expect(onApply).not.toHaveBeenCalled()

  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Cancel' }))
  })
  expect(onApply).not.toHaveBeenCalled()

  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Apply now' }))
  })
  await act(async () => {
    fireEvent.click(
      await view.findByRole('button', { name: 'Recreate container' })
    )
  })
  expect(onApply).toHaveBeenCalledTimes(1)
})

test('Apply now waits for unsaved edits to be saved', async () => {
  const view = renderView(STANDALONE)
  fireEvent.change(view.getByLabelText('Interval (s)'), {
    target: { value: '90' },
  })
  await waitFor(() =>
    expect(
      (view.getByRole('button', { name: 'Apply now' }) as HTMLButtonElement)
        .disabled
    ).toBe(true)
  )
  expect(view.getByText(/Save your changes first/)).toBeTruthy()
})

test('a worker database can be overridden but not recreated from here', () => {
  const view = renderView({
    ...STANDALONE,
    capability: {
      configurable: true,
      can_apply_now: false,
      apply_now_reason:
        'it runs on worker node 3; the settings apply when its container is next recreated from its workload spec',
    },
    running: undefined,
    pending_recreate: false,
  })
  expect(
    (view.getByRole('button', { name: 'Apply now' }) as HTMLButtonElement)
      .disabled
  ).toBe(true)
  expect(view.getByText(/runs on worker node 3/)).toBeTruthy()
  expect(view.getByLabelText('Interval (s)')).toBeTruthy()
})

test('a cluster explains why it has no configurable health check', () => {
  const view = renderView({
    ...STANDALONE,
    engine: undefined,
    capability: {
      configurable: false,
      reason:
        'its cluster members run without a Docker healthcheck; the cluster’s monitor watches them',
      can_apply_now: false,
    },
    effective: undefined,
    inherited: undefined,
    running: undefined,
    pending_recreate: false,
  })
  expect(view.getByText('Health check settings are not available')).toBeTruthy()
  expect(view.getByText(/cluster members run without/)).toBeTruthy()
  expect(view.queryByRole('button', { name: 'Apply now' })).toBeNull()
})

test('a host whose Docker ignores the start interval says so', () => {
  const view = renderView({ ...STANDALONE, start_interval_supported: false })
  expect(
    view.getByText("This host's Docker ignores the start interval")
  ).toBeTruthy()
})

test('a start period of 0 with a long interval is refused before Save', async () => {
  const onSave = mock((_: HealthcheckOverrides) => {})
  const view = renderView(STANDALONE, { onSave })
  fireEvent.change(view.getByLabelText('Interval (s)'), {
    target: { value: '120' },
  })
  fireEvent.change(view.getByLabelText('Start period (s)'), {
    target: { value: '0' },
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save overrides' }))
  })
  expect(await view.findByText(/must fit in the 90s Temps waits/)).toBeTruthy()
  expect(onSave).not.toHaveBeenCalled()
})

test('overrides set aside because they no longer combine say why', () => {
  const view = renderView({
    ...STANDALONE,
    overrides_ignored_reason:
      'Timeout (40s) from the instance default must be shorter than the interval (30s) from this service',
  })
  expect(
    view.getByText("This service's overrides are not being used")
  ).toBeTruthy()
  expect(
    view.getByText(/Timeout \(40s\) from the instance default/)
  ).toBeTruthy()
})

test('a stopped service cannot be recreated, and says when it applies', () => {
  const view = renderView({
    ...STANDALONE,
    capability: {
      configurable: true,
      can_apply_now: false,
      apply_now_reason:
        'it is stopped, and recreating its container would start it; the settings apply when its container is next created',
    },
  })
  expect(
    (view.getByRole('button', { name: 'Apply now' }) as HTMLButtonElement)
      .disabled
  ).toBe(true)
  expect(view.getByText(/it is stopped/)).toBeTruthy()
})
