// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import GitProviderDetail from './GitProviderDetail'

installBrowserWindow()
afterEach(cleanup)

for (const successful of [true, false]) {
  test(`reauthorization completion shows ${successful ? 'success' : 'account mismatch'} and removes callback secrets`, async () => {
    window.history.replaceState(
      null,
      '',
      '/git-providers/7#reauthorize_state=reauthorize.example-state&reauthorize_code=example-code'
    )
    let requestBody: unknown
    const api = mockApi({
      'GET /git-providers/7': () =>
        jsonResponse({
          id: 7,
          name: 'Example GitHub',
          provider_type: 'github',
          auth_method: 'oauth',
          is_active: true,
          is_default: false,
          created_at: '2026-01-01T00:00:00Z',
          updated_at: '2026-01-01T00:00:00Z',
        }),
      'GET /git-providers/7/connections': () => jsonResponse([]),
      'POST /git-connections/reauthorize/complete': async (request) => {
        requestBody = await request.json()
        return successful
          ? jsonResponse({ id: 11, provider_id: 7 })
          : jsonResponse(
              {
                detail:
                  'Connection 11 belongs to example-account, but authorization returned other-account.',
              },
              409
            )
      },
    })
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    const view = render(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={['/git-providers/7']}>
          <BreadcrumbProvider>
            <Routes>
              <Route
                path="/git-providers/:id"
                element={<GitProviderDetail />}
              />
            </Routes>
          </BreadcrumbProvider>
        </MemoryRouter>
      </QueryClientProvider>
    )
    expect(
      await view.findByText(
        successful
          ? 'Connection reauthorized. Projects, repositories and webhooks remain linked.'
          : /Reauthorization failed:.*example-account.*other-account/
      )
    ).toBeTruthy()
    expect(window.location.hash).toBe('')
    expect(requestBody).toEqual({
      state: 'reauthorize.example-state',
      code: 'example-code',
    })
    expect(api.calls.filter((call) => call.method === 'POST')).toHaveLength(1)
    client.clear()
    api.restore()
  })
}
