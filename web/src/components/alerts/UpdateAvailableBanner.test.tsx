// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render } from '@testing-library/react'
import {
  getUpdateStatusQueryKey,
  getUpdateCapabilityQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { UpdateAvailableBanner } from './UpdateAvailableBanner'

afterEach(() => {
  cleanup()
  localStorage.clear()
})

function notice(version = 'v1.2.0', security = true) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(getUpdateStatusQueryKey(), {
    update_available: true,
    latest_version: version,
    current_version: 'v1.1.0',
    security_update: security,
    release_url: 'https://example.invalid/notes',
    docs_url: 'https://example.invalid/upgrade',
  })
  client.setQueryData(getUpdateCapabilityQueryKey(), { allowed: false })
  return render(
    <QueryClientProvider client={client}>
      <UpdateAvailableBanner />
    </QueryClientProvider>
  )
}

test('security notice links to release notes and dismissal lasts only for its version', () => {
  let view = notice()
  expect(view.getByRole('alert').textContent).toContain(
    'Security update available'
  )
  expect(
    view.getByRole('link', { name: 'Release notes' }).getAttribute('href')
  ).toBe('https://example.invalid/notes')
  fireEvent.click(
    view.getByRole('button', { name: 'Dismiss update notification' })
  )
  expect(view.queryByRole('alert')).toBeNull()
  view.unmount()
  view = notice()
  expect(view.queryByRole('alert')).toBeNull()
  view.unmount()
  view = notice('v1.3.0')
  expect(view.getByRole('alert')).toBeTruthy()
})

test('ordinary releases keep the informational notice', () => {
  const view = notice('v1.2.0', false)
  expect(view.queryByRole('alert')).toBeNull()
  expect(view.getByRole('status').textContent).toContain('Update available')
})

test('a previously dismissed ordinary update cannot hide a new security marking', () => {
  localStorage.setItem('temps-update-banner-dismissed', 'v1.2.0')
  const view = notice()
  expect(view.getByRole('alert')).toBeTruthy()
})
