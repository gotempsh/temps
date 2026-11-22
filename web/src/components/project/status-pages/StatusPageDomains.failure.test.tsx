// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { toast } from 'sonner'
import type { StatusPageDomainResponse, StatusPageResponse } from '@/api/client'
import { announceMutationFailure } from '@/lib/mutation-error-toast'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { StatusPageDomains } from './StatusPageDomains'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const NOW = '2026-01-01T00:00:00Z'

const page = {
  id: 1,
  title: 'Status',
  slug: 'status',
  is_published: true,
  monitors: [],
  project_id: 1,
  public_url_status: 'unavailable',
  created_at: NOW,
  updated_at: NOW,
} as unknown as StatusPageResponse

function domain(status: StatusPageDomainResponse['status']) {
  return {
    id: 7,
    hostname: 'status.example.test',
    status,
    serving: false,
    verification_method: 'http-01',
    dns_records: [],
    created_at: NOW,
    last_error: null,
  } satisfies StatusPageDomainResponse
}

// The app's own default handler is module-private to App.tsx; it delegates to
// this function, so the test wires the same one.
function renderRow() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: {
        retry: false,
        onError: (error, _variables, _onMutateResult, context) =>
          announceMutationFailure(
            error as { title?: string; detail?: string },
            context
          ),
      },
    },
  })
  return render(
    <QueryClientProvider client={client}>
      <StatusPageDomains page={page} />
    </QueryClientProvider>
  )
}

const REFUSAL = {
  title: 'Hostname Unavailable',
  detail: "publish status page 1 before verifying 'status.example.test'",
}

test('a refused Verify shows the authored title and the server reason, and re-reads the list', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  let listReads = 0
  const api = mockApi({
    'GET /status-pages/1/domains': () => {
      listReads += 1
      return jsonResponse([domain('pending')])
    },
    'POST /status-pages/1/domains/7/verify': () => jsonResponse(REFUSAL, 409),
  })
  const view = renderRow()

  fireEvent.click(await view.findByRole('button', { name: 'Verify' }))

  await waitFor(() =>
    expect(error).toHaveBeenCalledWith(
      'Could not verify status.example.test yet',
      { description: REFUSAL.detail }
    )
  )
  // The failed attempt may have changed the row, so it is still re-read.
  await waitFor(() => expect(listReads).toBeGreaterThanOrEqual(2))
  api.restore()
})

test('a refused Renew shows the authored title and the server reason', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  const api = mockApi({
    'GET /status-pages/1/domains': () => jsonResponse([domain('active')]),
    'POST /status-pages/1/domains/7/renew': () =>
      jsonResponse(
        {
          title: 'Certificate Operation In Progress',
          detail: 'An operation is already running for status.example.test',
        },
        409
      ),
  })
  const view = renderRow()

  fireEvent.click(await view.findByRole('button', { name: 'Renew' }))

  await waitFor(() =>
    expect(error).toHaveBeenCalledWith(
      'Could not renew the certificate of status.example.test',
      { description: 'An operation is already running for status.example.test' }
    )
  )
  api.restore()
})
