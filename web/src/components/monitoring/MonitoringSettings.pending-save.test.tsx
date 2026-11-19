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
import { MemoryRouter, Route, Routes } from 'react-router'
import { Toaster, toast } from 'sonner'
import type { NotificationPreferencesResponse } from '@/api/client'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi, restoreAllMockApis } from '@/test/fetch-mock'
import { MonitoringSettings } from './MonitoringSettings'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
  restoreAllMockApis()
})

const PREFERENCES = {
  backup_failures_enabled: true,
  backup_successes_enabled: false,
  batch_similar_notifications: false,
  build_errors_enabled: true,
  deployment_failures_enabled: true,
  digest_sections: {
    performance: true,
    deployments: true,
    errors: true,
    funnels: true,
    projects: true,
  },
  digest_send_day: 'monday',
  digest_send_time: '09:00',
  dns_changes_enabled: false,
  domain_expiration_enabled: true,
  email_enabled: true,
  error_threshold: 10,
  error_time_window: 5,
  load_balancer_issues_enabled: false,
  minimum_severity: 'warning',
  retention_policy_violations_enabled: false,
  route_downtime_enabled: false,
  runtime_errors_enabled: false,
  slack_enabled: false,
  ssl_days_before_expiration: 30,
  ssl_expiration_enabled: true,
  weekly_digest_enabled: false,
} as unknown as NotificationPreferencesResponse

/** A save the test answers when it chooses, to look at the form meanwhile. */
function deferredSave() {
  let answer: (response: Response) => void = () => {}
  const pending = new Promise<Response>((resolve) => (answer = resolve))
  return { pending, answer }
}

function renderPage(save: Promise<Response>) {
  mockApi({
    'GET /notification-preferences': () => jsonResponse(PREFERENCES),
    'PUT /notification-preferences': () => save,
  })
  return render(
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
      <MemoryRouter initialEntries={['/monitoring/alerts']}>
        <Routes>
          <Route path="/monitoring/:section" element={<MonitoringSettings />} />
        </Routes>
        <Toaster />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

function switchFor(view: ReturnType<typeof render>, label: string) {
  return view.getByRole('switch', { name: label }) as HTMLButtonElement
}

/**
 * Whether the browser disables this control: its own `disabled`, or a
 * disabled `<fieldset>` around it (which the test DOM does not apply itself).
 */
function isDisabled(control: HTMLElement): boolean {
  return (
    (control as HTMLButtonElement).disabled ||
    control.closest('fieldset')?.disabled === true
  )
}

async function submitRouteDowntime(view: ReturnType<typeof render>) {
  await act(async () => {
    fireEvent.click(await view.findByRole('switch', { name: 'Route downtime' }))
  })
  await act(async () => {
    fireEvent.click(view.getByRole('button', { name: 'Save route alerts' }))
  })
}

test('the form cannot be edited while its save is pending', async () => {
  const save = deferredSave()
  const view = renderPage(save.pending)
  await submitRouteDowntime(view)

  // A later edit would be overwritten by the reset when the save completes,
  // so the controls are disabled until it does.
  await waitFor(() =>
    expect(isDisabled(switchFor(view, 'Load balancer issues'))).toBe(true)
  )
  expect(
    (
      view.getByRole('button', {
        name: 'Save route alerts',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)

  await act(async () => {
    save.answer(jsonResponse(PREFERENCES))
  })
  await waitFor(() =>
    expect(isDisabled(switchFor(view, 'Load balancer issues'))).toBe(false)
  )
  expect(
    (
      view.getByRole('button', {
        name: 'Save route alerts',
      }) as HTMLButtonElement
    ).disabled
  ).toBe(true)
  expect(switchFor(view, 'Route downtime').getAttribute('aria-checked')).toBe(
    'true'
  )
})

test('a refused save keeps the change unsaved so it can be retried', async () => {
  const save = deferredSave()
  const view = renderPage(save.pending)
  await submitRouteDowntime(view)
  await act(async () => {
    save.answer(
      jsonResponse(
        {
          title: 'Forbidden',
          status: 403,
          detail: 'Requires notifications:write permission',
        },
        403
      )
    )
  })

  expect(
    await view.findByText(
      'Failed to save route alert settings: Requires notifications:write permission'
    )
  ).toBeTruthy()
  // Still dirty: the Save button stays available and the edit is kept.
  await waitFor(() =>
    expect(
      (
        view.getByRole('button', {
          name: 'Save route alerts',
        }) as HTMLButtonElement
      ).disabled
    ).toBe(false)
  )
  expect(switchFor(view, 'Route downtime').getAttribute('aria-checked')).toBe(
    'true'
  )
})
