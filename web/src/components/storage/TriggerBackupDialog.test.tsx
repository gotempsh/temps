// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { TriggerBackupDialog } from './TriggerBackupDialog'

installBrowserWindow()
afterEach(cleanup)

function renderDialog() {
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
      <TriggerBackupDialog
        open
        onOpenChange={() => {}}
        serviceId={7}
        serviceName="synthetic-db"
      />
    </QueryClientProvider>
  )
}

const forbidden = () =>
  jsonResponse(
    {
      title: 'Forbidden',
      status: 403,
      detail:
        'Access denied to global backup resources: Only an administrator may list S3 backup sources',
    },
    403
  )

test('a role refused the destination list can still back up to the default one', async () => {
  let sent: Record<string, unknown> | undefined
  const api = mockApi({
    'GET /backups/s3-sources': forbidden,
    'GET /backups/external-services/7/run-preflight': () =>
      jsonResponse({ engine: 'postgres_walg', restarts_database: false }),
    'POST /backups/external-services/7/run': async (request) => {
      sent = (await request.json()) as Record<string, unknown>
      return jsonResponse(
        {
          backup_id: 1,
          restarts_database: false,
        },
        202
      )
    },
  })
  const view = renderDialog()

  // A neutral note, not an error that says to ask an administrator.
  await view.findByText(/Your role cannot choose a backup destination/)
  expect(view.queryByText(/Could not load backup destinations/)).toBeNull()
  expect(view.queryByRole('button', { name: /Retry/ })).toBeNull()
  expect(view.queryByText('Storage destination')).toBeNull()

  const start = view.getByRole('button', {
    name: 'Start backup',
  }) as HTMLButtonElement
  await waitFor(() => expect(start.disabled).toBe(false))
  fireEvent.click(start)

  await waitFor(() => expect(sent).toBeDefined())
  // No destination is named, so the server uses the default one.
  expect(sent).toMatchObject({ backup_type: 'full' })
  expect(sent).not.toHaveProperty('s3_source_id')
  api.restore()
})
