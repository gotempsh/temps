// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { ShieldProposalsPanel } from './ShieldProposalsPanel'

installBrowserWindow()
afterEach(cleanup)

const PROPOSAL = {
  id: 41,
  kind: 'ip',
  subject: '203.0.113.7',
  verdict: 'block',
  reason: 'exploit CVE-2021-41773',
  hits: 3,
  first_seen: '2026-10-27T09:00:00Z',
  last_seen: '2026-10-27T10:00:00Z',
  expires_at: '2026-11-03T10:00:00Z',
  status: 'pending',
  decided_at: null,
  decided_by: null,
}

function renderPanel(learning = true) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <ShieldProposalsPanel learning={learning} />
    </QueryClientProvider>
  )
}

test('a pending proposal is approved and leaves the pending list', async () => {
  let status = 'pending'
  const statusQueries: Array<string | null> = []
  const api = mockApi({
    'GET /settings/shield/proposals': (request) => {
      const wanted = new URL(request.url).searchParams.get('status')
      statusQueries.push(wanted)
      const rows = [{ ...PROPOSAL, status }]
      return jsonResponse(
        rows.filter((row) => !wanted || row.status === wanted)
      )
    },
    'POST /settings/shield/proposals/41/approve': () => {
      status = 'approved'
      return jsonResponse({ ...PROPOSAL, status })
    },
  })
  const view = renderPanel()

  await view.findByText('203.0.113.7')
  expect(statusQueries[0]).toBe('pending')
  view.getByText('exploit CVE-2021-41773')
  fireEvent.click(view.getByRole('button', { name: 'Approve 203.0.113.7' }))

  await view.findByText('No proposals to review')
  expect(api.called('POST', '/settings/shield/proposals/41/approve')).toBe(true)
  expect(api.called('POST', '/settings/shield/proposals/41/reject')).toBe(false)
  api.restore()
})

test('an approved proposal can be rejected, and offers no second approval', async () => {
  const api = mockApi({
    // The server answers whatever the filter; the row's state drives the buttons.
    'GET /settings/shield/proposals': () =>
      jsonResponse([{ ...PROPOSAL, status: 'approved' }]),
    'POST /settings/shield/proposals/41/reject': () =>
      jsonResponse({ ...PROPOSAL, status: 'rejected' }),
  })
  const view = renderPanel()

  await view.findByText('approved')
  expect(view.queryByRole('button', { name: 'Approve 203.0.113.7' })).toBeNull()
  fireEvent.click(view.getByRole('button', { name: 'Reject 203.0.113.7' }))
  await waitFor(() =>
    expect(api.called('POST', '/settings/shield/proposals/41/reject')).toBe(
      true
    )
  )
  api.restore()
})

test('with learning off, the panel says approvals do not apply', async () => {
  const api = mockApi({
    'GET /settings/shield/proposals': () => jsonResponse([PROPOSAL]),
  })
  const view = renderPanel(false)

  await view.findByText('203.0.113.7')
  view.getByText(/Learning is off, so approved proposals do not apply/)
  api.restore()
})
