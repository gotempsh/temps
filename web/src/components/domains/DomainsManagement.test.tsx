// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { MemoryRouter, Route, Routes, useParams } from 'react-router'
import type { DomainResponse } from '@/api/client/types.gen'
import { PlatformAccessContext } from '@/contexts/PlatformAccessContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { DomainsManagement } from './DomainsManagement'

installBrowserWindow()
afterEach(cleanup)

const domain: DomainResponse = {
  id: 12,
  domain: 'app.example.com',
  status: 'active',
  is_wildcard: false,
  verification_method: 'http-01',
  created_at: 1_767_225_600_000,
  updated_at: 1_767_225_600_000,
  expiration_time: 1_798_761_600_000,
}

function DomainPage() {
  return <p>Opened domain {useParams().id}</p>
}

function renderTable() {
  // Tunnels and capabilities answer 404: the table still lists certificates.
  mockApi({})
  return render(
    <QueryClientProvider
      client={
        new QueryClient({ defaultOptions: { queries: { retry: false } } })
      }
    >
      <MemoryRouter initialEntries={['/domains']}>
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
          <Routes>
            <Route
              path="/domains"
              element={
                <DomainsManagement
                  domains={[domain]}
                  sort="domain"
                  direction="asc"
                  onSortChange={() => {}}
                  isError={false}
                  isLoading={false}
                  reloadDomains={() => {}}
                  total={1}
                  page={1}
                  pageSize={20}
                  totalPages={1}
                  onPageChange={() => {}}
                  searchQuery=""
                  onSearchChange={() => {}}
                  isSearching={false}
                />
              }
            />
            <Route path="/domains/:id" element={<DomainPage />} />
          </Routes>
        </PlatformAccessContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

test('the domain name links to the domain, and a click on the row follows it', async () => {
  const view = renderTable()

  const link = view.getByRole('link', { name: 'app.example.com' })
  expect(link.getAttribute('href')).toBe('/domains/12')
  // The name reads as the record's name, not as a link (DESIGN.md).
  expect(link.className.split(' ')).toContain('no-underline')
  expect(link.querySelector('svg')).toBeNull()
  // DESIGN.md: no link overlay across the row; the row follows the link.
  expect(link.className).not.toContain('after:')
  fireEvent.click(link.closest('td') as HTMLElement)
  await waitFor(() => view.getByText('Opened domain 12'))
})
