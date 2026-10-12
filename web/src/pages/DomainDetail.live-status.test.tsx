// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { PlatformAccessContext } from '@/contexts/PlatformAccessContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DomainDetail } from './DomainDetail'

installBrowserWindow()
afterEach(cleanup)

const TIMESTAMP = Date.parse('2026-01-01T00:00:00Z')

function domain(status: string, lastError: string | null = null) {
  return {
    id: 1,
    domain: 'app.example.test',
    status,
    verification_method: 'dns-01',
    is_wildcard: false,
    last_error: lastError,
    last_error_type: lastError ? 'challenge_validation' : null,
    created_at: TIMESTAMP,
    updated_at: TIMESTAMP,
  }
}

function order(status: string) {
  return {
    id: 2,
    domain_id: 1,
    identifiers: [],
    order_url: 'https://acme.example.test/order/2',
    status,
    email: 'operator@example.test',
    authorizations: {
      challenge_type: 'dns-01',
      dns_txt_records: [
        { name: '_acme-challenge.app.example.test', value: 'token-value' },
      ],
      key_authorization: 'key-authorization',
      token: 'token',
      validation_url: 'https://acme.example.test/chall/1',
    },
    created_at: TIMESTAMP,
    updated_at: TIMESTAMP,
  }
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/domains/1']}>
        <PlatformAccessContext.Provider
          value={{
            accessInfo: undefined,
            isLoading: false,
            error: null,
            refetch: () => {},
            isLocal: false,
            isNat: false,
            isCloudflare: false,
            isDirect: true,
          }}
        >
          <BreadcrumbProvider>
            <Routes>
              <Route path="/domains/:id" element={<DomainDetail />} />
            </Routes>
          </BreadcrumbProvider>
        </PlatformAccessContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('a failed verification shows the failed status without a reload', async () => {
  // The API reports `failed` once the finalize call has been made; until then
  // the domain is still `challenge_requested`.
  let finalized = false
  const api = mockApi({
    'GET /domains/1': () =>
      jsonResponse(
        finalized
          ? domain('failed', 'DNS validation failed for the challenge record')
          : domain('challenge_requested')
      ),
    'GET /domains/1/order': () =>
      jsonResponse(order(finalized ? 'invalid' : 'pending')),
    'POST /domains/1/order/finalize': () => {
      finalized = true
      return jsonResponse(
        {
          title: 'ACME Error',
          detail: 'DNS validation failed for the challenge record',
        },
        400
      )
    },
  })
  const view = renderPage()

  // The header action and the step panel both offer "Verify & finalize".
  const [verify] = await view.findAllByRole('button', {
    name: /Verify & finalize/,
  })
  expect(view.queryByText('failed')).toBeNull()
  fireEvent.click(verify!)

  // Well inside the 3 s polling interval: this is the post-mutation re-read,
  // not the poll.
  await waitFor(() => expect(view.getAllByText('failed').length).toBeGreaterThan(0), {
    timeout: 2000,
  })
  expect(view.queryByText('challenge_requested')).toBeNull()
  expect(view.getByText(/Retry verification/)).toBeTruthy()
  api.restore()
})

test('an in-flight domain is polled and polling stops once it is settled', async () => {
  let reads = 0
  const api = mockApi({
    'GET /domains/1': () => {
      reads += 1
      return jsonResponse(
        reads < 2 ? domain('challenge_requested') : domain('failed', 'rejected')
      )
    },
    'GET /domains/1/order': () => jsonResponse(order('invalid')),
  })
  const view = renderPage()

  await view.findAllByText('challenge_requested')
  // No click: the page has to notice the status change on its own.
  await waitFor(() => expect(view.getAllByText('failed').length).toBeGreaterThan(0), {
    timeout: 6000,
  })
  const readsWhenSettled = reads

  await new Promise((resolve) => setTimeout(resolve, 3500))
  expect(reads).toBe(readsWhenSettled)
  api.restore()
}, 15000)
