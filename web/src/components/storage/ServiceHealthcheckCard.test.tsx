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
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter } from 'react-router'
import { getServiceHealthcheckQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type {
  HealthcheckOverrides,
  HealthcheckTimings,
  ServiceHealthcheck,
} from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import {
  ServiceHealthcheckCard,
  ServiceHealthcheckView,
  type ServiceHealthcheckViewProps,
} from './ServiceHealthcheckCard'

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

function view(
  healthcheck: ServiceHealthcheck,
  props: Partial<Omit<ServiceHealthcheckViewProps, 'healthcheck'>> = {}
) {
  return (
    <MemoryRouter>
      <ServiceHealthcheckView
        onSave={() => {}}
        onApply={() => {}}
        {...props}
        healthcheck={healthcheck}
      />
    </MemoryRouter>
  )
}

function renderView(
  healthcheck: ServiceHealthcheck,
  props: Partial<Omit<ServiceHealthcheckViewProps, 'healthcheck'>> = {}
) {
  return render(view(healthcheck, props))
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

test('a host whose Docker ignores the start interval shows no running start interval', () => {
  // Such a daemon is sent the start interval as the interval, and reads the
  // start interval back as 0.
  const view = renderView({
    ...STANDALONE,
    running: { ...BUILTIN, interval_secs: 2, start_interval_secs: 0 },
    pending_recreate: false,
    start_interval_supported: false,
  })
  expect(view.getByText('Applied')).toBeTruthy()
  expect(view.queryByText('Running: 0s')).toBeNull()
  expect(view.getByText('Running: 2s')).toBeTruthy()
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
  expect(
    await view.findByText(/would end after the 90s Temps waits/)
  ).toBeTruthy()
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

test('only a viewer who can read the instance default gets a link to it', () => {
  const member = renderView(STANDALONE)
  expect(member.queryByRole('link', { name: 'Instance default' })).toBeNull()
  expect(
    member.getByText(/An administrator sets the instance default/)
  ).toBeTruthy()
  cleanup()

  const admin = renderView(STANDALONE, { canReadDefaults: true })
  expect(
    admin.getByRole('link', { name: 'Instance default' }).getAttribute('href')
  ).toBe('/settings/service-healthchecks')
})

test("another service's settings do not keep this service's edits", async () => {
  const rendered = renderView(STANDALONE)
  fireEvent.change(rendered.getByLabelText('Interval (s)'), {
    target: { value: '120' },
  })
  await waitFor(() =>
    expect(rendered.getByText('Discard changes')).toBeTruthy()
  )

  // The page navigates to another service and the card is reused.
  rendered.rerender(
    view({
      ...STANDALONE,
      service_id: 8,
      service_name: 'sessions-cache',
      overrides: { ...NO_OVERRIDES, interval_secs: 300 },
    })
  )

  await waitFor(() =>
    expect(
      (rendered.getByLabelText('Interval (s)') as HTMLInputElement).value
    ).toBe('300')
  )
  expect(rendered.queryByText('Discard changes')).toBeNull()
})

test('a failed save is shown beside Save and cleared by the next edit', async () => {
  const onEdit = mock(() => {})
  const rendered = renderView(STANDALONE, {
    saveError: 'Interval (45s) is not allowed here',
    onEdit,
  })
  expect(
    rendered.getByText('The health check settings were not saved')
  ).toBeTruthy()
  expect(rendered.getByText('Interval (45s) is not allowed here')).toBeTruthy()

  fireEvent.change(rendered.getByLabelText('Interval (s)'), {
    target: { value: '90' },
  })
  await waitFor(() => expect(onEdit).toHaveBeenCalled())
})

test("a save that finishes after another service's page opened stays with its own service", async () => {
  const other: ServiceHealthcheck = {
    ...STANDALONE,
    service_id: 8,
    service_name: 'sessions-cache',
    overrides: { ...NO_OVERRIDES, interval_secs: 300 },
  }
  const saved: ServiceHealthcheck = {
    ...STANDALONE,
    overrides: { ...NO_OVERRIDES, interval_secs: 120 },
  }
  let finishSave: (response: Response) => void = () => {}
  const api = mockApi({
    'GET /external-services/7/healthcheck': () => jsonResponse(STANDALONE),
    'GET /external-services/8/healthcheck': () => jsonResponse(other),
    'PUT /external-services/7/healthcheck': () =>
      new Promise<Response>((resolve) => (finishSave = resolve)),
  })
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const card = (serviceId: number, serviceName: string) => (
    <QueryClientProvider client={queryClient}>
      <MemoryRouter>
        <ServiceHealthcheckCard
          serviceId={serviceId}
          serviceName={serviceName}
        />
      </MemoryRouter>
    </QueryClientProvider>
  )
  try {
    const rendered = render(card(7, 'orders-cache'))
    const interval = (await rendered.findByLabelText(
      'Interval (s)'
    )) as HTMLInputElement
    fireEvent.change(interval, { target: { value: '120' } })
    await act(async () => {
      fireEvent.click(rendered.getByRole('button', { name: 'Save overrides' }))
    })
    await waitFor(() =>
      expect(api.called('PUT', '/external-services/7/healthcheck')).toBe(true)
    )

    // The page moves to another service while the save is in flight.
    rendered.rerender(card(8, 'sessions-cache'))
    await waitFor(() =>
      expect(
        (rendered.getByLabelText('Interval (s)') as HTMLInputElement).value
      ).toBe('300')
    )
    // Its fields are not locked by the other service's save.
    expect(
      (rendered.getByLabelText('Interval (s)') as HTMLInputElement).disabled
    ).toBe(false)

    await act(async () => finishSave(jsonResponse(saved)))
    const key = (id: number) => getServiceHealthcheckQueryKey({ path: { id } })
    await waitFor(() =>
      expect(
        queryClient.getQueryData<ServiceHealthcheck>(key(7))?.overrides
          .interval_secs
      ).toBe(120)
    )
    expect(
      queryClient.getQueryData<ServiceHealthcheck>(key(8))?.overrides
        .interval_secs
    ).toBe(300)
    expect(
      (rendered.getByLabelText('Interval (s)') as HTMLInputElement).value
    ).toBe('300')
  } finally {
    api.restore()
    queryClient.clear()
  }
})
