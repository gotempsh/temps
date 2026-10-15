// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, spyOn, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { toast } from 'sonner'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { PlatformAccessContext } from '@/contexts/PlatformAccessContext-shared'
import { announceMutationFailure } from '@/lib/mutation-error-toast'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { DomainDetail } from './DomainDetail'

installBrowserWindow()
afterEach(() => {
  cleanup()
  spyOn(toast, 'error').mockRestore()
})

const TIMESTAMP = Date.parse('2026-01-01T00:00:00Z')

const pendingDomain = {
  id: 1,
  domain: 'app.example.test',
  status: 'pending',
  verification_method: 'dns-01',
  is_wildcard: false,
  last_error: null,
  last_error_type: null,
  created_at: TIMESTAMP,
  updated_at: TIMESTAMP,
}

function renderPage() {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false },
      mutations: {
        retry: false,
        // Same function the app-wide handler in App.tsx delegates to.
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

test('a failed Create order shows the authored title and the server reason, and re-reads the domain', async () => {
  const error = spyOn(toast, 'error').mockImplementation(() => 'id')
  let domainReads = 0
  const api = mockApi({
    'GET /domains/1': () => {
      domainReads += 1
      return jsonResponse(pendingDomain)
    },
    'GET /domains/1/order': () => jsonResponse({ order: null, reason: null }),
    'POST /domains/1/order': () =>
      jsonResponse(
        {
          title: 'Internal Server Error',
          detail: 'ACME directory is unreachable; try again later',
        },
        500
      ),
  })
  const view = renderPage()

  const [createOrder] = await view.findAllByRole('button', {
    name: /Create order/,
  })
  const readsBefore = domainReads
  fireEvent.click(createOrder!)

  await waitFor(() =>
    expect(error).toHaveBeenCalledWith('Failed to create ACME order', {
      description: 'ACME directory is unreachable; try again later',
    })
  )
  await waitFor(() => expect(domainReads).toBeGreaterThan(readsBefore))
  api.restore()
})
