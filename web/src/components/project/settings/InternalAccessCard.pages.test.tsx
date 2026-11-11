// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import type { InternalAccessGrantResponse } from '@/api/client/types.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { InternalAccessCard } from './InternalAccessCard'

installBrowserWindow()
afterEach(cleanup)

function grant(id: number): InternalAccessGrantResponse {
  return {
    id,
    target_project_id: 1,
    target_project_name: 'Orders API',
    target_project_slug: 'orders-api',
    target_environment_id: null,
    target_environment_name: null,
    caller_project_id: 100 + id,
    caller_project_name: `Caller ${id}`,
    caller_project_slug: `caller-${id}`,
    caller_environment_id: null,
    caller_environment_name: null,
    description: null,
    created_by: 9,
    created_at: '2026-11-09T10:00:00Z',
  }
}

/** A server holding `count` grants, paged as the API pages them. */
function grantsServer(count: number) {
  let grants = Array.from({ length: count }, (_, i) => grant(i + 1))
  return mockApi({
    'GET /projects/1/internal-access': (request) => {
      const url = new URL(request.url)
      const page = Number(url.searchParams.get('page') ?? '1')
      const pageSize = Number(url.searchParams.get('page_size') ?? '20')
      return jsonResponse({
        items: grants.slice((page - 1) * pageSize, page * pageSize),
        total: grants.length,
        page,
        page_size: pageSize,
      })
    },
    'DELETE /projects/1/internal-access/21': () => {
      grants = grants.filter((g) => g.id !== 21)
      return new Response(null, { status: 204 })
    },
  })
}

/**
 * The reported failure: with 21 grants, removing the only one on page 2
 * left the card on page 2, which no longer exists. The refreshed page was
 * empty and, with a single page left, the pager was hidden, so the other
 * 20 grants could not be reached until the card was reopened.
 */
test('removing the last grant of the last page goes back to a page that has grants', async () => {
  grantsServer(21)
  render(
    <MemoryRouter>
      <QueryClientProvider
        client={
          new QueryClient({ defaultOptions: { queries: { retry: false } } })
        }
      >
        <InternalAccessCard
          projectId={1}
          projectName="Orders API"
          projectSlug="orders-api"
        />
      </QueryClientProvider>
    </MemoryRouter>
  )

  await screen.findByText('Caller 1, all environments')
  fireEvent.click(screen.getAllByLabelText('Next page')[0])
  await screen.findByText('Caller 21, all environments')

  fireEvent.click(
    screen.getByLabelText(
      'Remove internal access for Caller 21, all environments'
    )
  )
  const dialog = await screen.findByRole('alertdialog')
  fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }))

  await waitFor(() =>
    expect(screen.getByText('Caller 20, all environments')).toBeTruthy()
  )
  expect(screen.queryByText('Caller 21, all environments')).toBeNull()
  expect(screen.getAllByRole('listitem').length).toBe(20)
})
