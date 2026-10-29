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

for (const action of ['retry', 'manual'] as const) {
  test(`retained cleanup order supports ${action} recovery without issuing a new certificate`, async () => {
    const domain = {
      id: 1,
      domain: 'app.example.test',
      status: 'active',
      verification_method: 'dns-01',
      is_wildcard: false,
      created_at: Date.now(),
      updated_at: Date.now(),
    }
    let retained = true
    let failCleanup = true
    const api = mockApi({
      'GET /domains/1': () => jsonResponse(domain),
      'GET /domains/1/order': () =>
        jsonResponse({
          order: retained
            ? {
                id: 2,
                domain_id: 1,
                status: 'valid',
                identifiers: [],
                order_url: 'https://acme.example.test/order/2',
                email: 'operator@example.test',
                created_at: domain.created_at,
                updated_at: domain.updated_at,
                authorizations: {
                  challenge_type: 'dns-01',
                  temps_dns_cleanup: {
                    provider_id: 3,
                    zone: 'example.test',
                    records: [
                      {
                        name: '_acme-challenge.app.example.test',
                        value: 'old-txt-value',
                        record_id: 'record-1',
                      },
                    ],
                  },
                },
              }
            : null,
        }),
      'POST /domains/1/order/finalize': () => {
        if (failCleanup)
          return jsonResponse({ detail: 'DNS provider unavailable' }, 502)
        retained = false
        return jsonResponse(domain)
      },
      'DELETE /domains/1/order': () => {
        retained = false
        return jsonResponse(domain)
      },
    })
    const client = new QueryClient({
      defaultOptions: {
        queries: { retry: false, staleTime: Infinity },
        mutations: { retry: false },
      },
    })
    const view = render(
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
    try {
      await view.findByText('DNS cleanup pending')
      expect(view.getByText(/old-txt-value/)).toBeTruthy()
      if (action === 'retry') {
        fireEvent.click(view.getByRole('button', { name: 'Retry DNS cleanup' }))
        await waitFor(() =>
          expect(api.called('POST', '/domains/1/order/finalize')).toBe(true)
        )
        await waitFor(() =>
          expect(
            view
              .getByRole('button', { name: 'Retry DNS cleanup' })
              .getAttribute('aria-disabled')
          ).toBe('false')
        )
        expect(view.getByText('DNS cleanup pending')).toBeTruthy()
        failCleanup = false
        fireEvent.click(view.getByRole('button', { name: 'Retry DNS cleanup' }))
      } else {
        fireEvent.click(
          view.getByRole('button', { name: 'Clear cleanup receipt' })
        )
        await view.findByRole('alertdialog')
        expect(api.called('DELETE', '/domains/1/order')).toBe(false)
        fireEvent.click(
          view.getByRole('button', { name: 'Records removed, clear receipt' })
        )
        await waitFor(() =>
          expect(api.called('DELETE', '/domains/1/order')).toBe(true)
        )
      }
      await waitFor(() =>
        expect(view.queryByText('DNS cleanup pending')).toBeNull()
      )
      expect(view.getByText('Active TLS certificate')).toBeTruthy()
      expect(api.called('POST', '/domains/1/order')).toBe(false)
      expect(api.calls.some((call) => call.path.endsWith('/renew'))).toBe(false)
    } finally {
      view.unmount()
      client.clear()
      api.restore()
    }
  })
}
