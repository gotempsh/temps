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
import type { ComposeHostStatusResponse } from '@/api/client'
import { getComposeStatusQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { ComposeVersionAlert } from './ComposeVersionAlert'

const clients: QueryClient[] = []
afterEach(() => {
  cleanup()
  clients.splice(0).forEach((client) => client.clear())
})

const unsupported: ComposeHostStatusResponse = {
  probed: true,
  available: true,
  supported: false,
  version: 'v2.32.4',
  minimum_version: '2.35.0',
  missing_flags: ['--no-env-resolution'],
  problem: 'Docker Compose v2.32.4 does not support --no-env-resolution',
  checked_at: '2026-10-17T00:00:00Z',
}

function renderAlert(status = unsupported) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  clients.push(client)
  client.setQueryData(getComposeStatusQueryKey(), status)
  return {
    client,
    ...render(
      <QueryClientProvider client={client}>
        <ComposeVersionAlert />
      </QueryClientProvider>
    ),
  }
}

test('the warning opens a guide with the detected version and copyable update and verification commands', () => {
  const view = renderAlert()
  fireEvent.click(view.getByRole('button', { name: 'Update Docker Compose' }))
  const dialog = view.getByRole('dialog', { name: 'Update Docker Compose' })
  expect(dialog.textContent).toContain('v2.32.4')
  expect(dialog.textContent).toContain('v2.35.0 or newer')
  expect(dialog.textContent).toContain(
    'sudo apt-get update && sudo apt-get install docker-compose-plugin'
  )
  expect(dialog.textContent).toContain('docker compose version')
  expect(dialog.textContent).toContain('same user')
  expect(dialog.textContent).toContain('Restart Temps')
  expect(
    view.getByRole('button', { name: 'Copy Compose update command' })
  ).toBeTruthy()
  expect(
    view.getByRole('button', { name: 'Copy Compose version command' })
  ).toBeTruthy()

  fireEvent.click(view.getAllByRole('button', { name: 'Close' })[0])
  expect(view.queryByRole('dialog')).toBeNull()
  expect(view.getByText('Docker Compose needs an upgrade')).toBeTruthy()
})

test('switching installation changes the instructions without offering host package commands inside containers', () => {
  const view = renderAlert()
  fireEvent.click(view.getByRole('button', { name: 'Update Docker Compose' }))
  const dialog = view.getByRole('dialog')

  fireEvent.click(view.getByRole('button', { name: 'Fedora / RHEL' }))
  expect(dialog.textContent).toContain(
    'sudo dnf install --refresh docker-compose-plugin'
  )
  expect(dialog.textContent).not.toContain('apt-get')

  fireEvent.click(view.getByRole('button', { name: 'Docker Desktop' }))
  expect(dialog.textContent).toContain('Check for updates')
  expect(dialog.textContent).not.toContain('sudo')

  fireEvent.click(view.getByRole('button', { name: 'Temps in Docker' }))
  expect(dialog.textContent).toContain('image that runs Temps')
  expect(dialog.textContent).toContain('persistent volumes')
  expect(dialog.textContent).toContain('inside the container')
  expect(
    view.queryByRole('button', { name: 'Copy Compose update command' })
  ).toBeNull()
})

test('a missing CLI still offers installation help and uses the server minimum version', () => {
  const view = renderAlert({
    ...unsupported,
    available: false,
    version: null,
    problem: null,
    minimum_version: '2.40.0',
  })
  fireEvent.click(view.getByRole('button', { name: 'Update Docker Compose' }))
  expect(view.getByRole('dialog').textContent).toContain('v2.40.0 or newer')
  expect(view.getByRole('dialog').textContent).not.toContain('Detected version')
})

test('a supported status clears the warning after the restarted server is refreshed', async () => {
  const view = renderAlert()
  await act(async () => {
    view.client.setQueryData(getComposeStatusQueryKey(), {
      ...unsupported,
      supported: true,
      missing_flags: [],
      problem: null,
    })
  })
  await waitFor(() =>
    expect(
      view.queryByRole('button', { name: 'Update Docker Compose' })
    ).toBeNull()
  )
})

test('dismissing the warning hides its update action', () => {
  const view = renderAlert()
  fireEvent.click(
    view.getByRole('button', { name: 'Dismiss Docker Compose warning' })
  )
  expect(
    view.queryByRole('button', { name: 'Update Docker Compose' })
  ).toBeNull()
})
