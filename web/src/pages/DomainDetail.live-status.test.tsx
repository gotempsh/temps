// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, beforeEach, expect, test } from 'bun:test'
import {
  QueryClient,
  QueryClientProvider,
  environmentManager,
  focusManager,
  onlineManager,
} from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { PlatformAccessContext } from '@/contexts/PlatformAccessContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DomainDetail } from './DomainDetail'

installBrowserWindow()
// Polling is skipped when React Query believes it runs on a server, and pauses
// while the document is hidden or offline. All three are process-wide
// singletons that other test files in the same run leave toggled, so pin them
// for this file and put them back.
const wasServer = environmentManager.isServer()
beforeEach(() => {
  environmentManager.setIsServer(() => false)
  focusManager.setFocused(true)
  onlineManager.setOnline(true)
})
afterEach(() => {
  cleanup()
  environmentManager.setIsServer(() => wasServer)
  focusManager.setFocused(undefined)
  onlineManager.setOnline(true)
})

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

// `GET /domains/{id}/order` wraps the order: `order` is null when none exists.
function orderBody(status: string) {
  return { order: order(status), reason: null }
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
      jsonResponse(orderBody(finalized ? 'invalid' : 'pending')),
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
  await waitFor(
    () => expect(view.getAllByText('failed').length).toBeGreaterThan(0),
    {
      timeout: 2000,
    }
  )
  expect(view.queryByText('challenge_requested')).toBeNull()
  // The order was refreshed along with the domain, so the CA's `invalid`
  // verdict is known and the retry offered is a new order, not another verify.
  expect(await view.findByText(/Start new order/)).toBeTruthy()
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
    'GET /domains/1/order': () => jsonResponse(orderBody('invalid')),
  })
  const view = renderPage()

  await view.findAllByText('challenge_requested')
  // No click: the page has to notice the status change on its own.
  await waitFor(
    () => expect(view.getAllByText('failed').length).toBeGreaterThan(0),
    {
      timeout: 12_000,
    }
  )
  const readsWhenSettled = reads

  await new Promise((resolve) => setTimeout(resolve, 3500))
  expect(reads).toBe(readsWhenSettled)
  api.restore()
}, 30_000)

test('the order is read again after the domain settles, so it never stays pending beside a failed badge', async () => {
  // The domain poll sees `failed` first; the order only reports `invalid` on
  // its next read. Following the domain status alone would stop polling the
  // order at that point and leave it `pending`.
  let domainReads = 0
  let orderReadsAfterFailed = 0
  const api = mockApi({
    'GET /domains/1': () => {
      domainReads += 1
      return jsonResponse(
        domainReads < 2 ? domain('challenge_requested') : domain('failed', 'x')
      )
    },
    'GET /domains/1/order': () => {
      if (domainReads < 2) return jsonResponse(orderBody('pending'))
      orderReadsAfterFailed += 1
      return jsonResponse(
        orderBody(orderReadsAfterFailed < 2 ? 'pending' : 'invalid')
      )
    },
  })
  const view = renderPage()

  await view.findAllByText('challenge_requested')
  await waitFor(
    () => expect(view.getAllByText('failed').length).toBeGreaterThan(0),
    {
      timeout: 12_000,
    }
  )
  // The first order read after `failed` still says `pending`; the catch-up
  // poll has to read it again to learn it is `invalid`.
  await waitFor(() => expect(orderReadsAfterFailed).toBeGreaterThanOrEqual(2), {
    timeout: 12_000,
  })
  expect(orderReadsAfterFailed).toBeGreaterThanOrEqual(2)
  api.restore()
}, 40_000)

test('certificate settings open separately and leave status and renewal history on the detail page', async () => {
  const api = mockApi({
    'GET /domains/1': () => jsonResponse(domain('active')),
    'GET /domains/1/order': () => jsonResponse({ order: null, reason: null }),
    'GET /domains/1/certificate-settings': () =>
      jsonResponse({
        challenge_type: 'http-01',
        renewal_days: 30,
        alert_days: 7,
        alerts_enabled: true,
      }),
  })
  try {
    const view = renderPage()
    const trigger = await view.findByRole('button', {
      name: 'Certificate settings',
    })
    expect(view.getByText('Active TLS certificate')).toBeTruthy()
    expect(view.getByText('Renewal history')).toBeTruthy()
    expect(view.queryByLabelText('Renew before expiry (days)')).toBeNull()
    expect(view.queryByRole('dialog')).toBeNull()

    fireEvent.click(trigger)
    expect(
      await view.findByRole('dialog', { name: 'Certificate settings' })
    ).toBeTruthy()
    expect(
      await view.findByLabelText('Renew before expiry (days)')
    ).toBeTruthy()
    expect(view.getByLabelText('Alert before expiry (days)')).toBeTruthy()

    fireEvent.click(view.getByRole('button', { name: /^Close$/ }))
    await waitFor(() => expect(view.queryByRole('dialog')).toBeNull())
    expect(view.getByText('Renewal history')).toBeTruthy()
    expect(view.queryByLabelText('Renew before expiry (days)')).toBeNull()
  } finally {
    api.restore()
  }
})
