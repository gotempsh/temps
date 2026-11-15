// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { cleanup, render } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { PlatformAccessContext } from '@/contexts/PlatformAccessContext-shared'
import { installBrowserWindow } from '@/test/dom'
import { mockApi } from '@/test/fetch-mock'
import { DNSConfigurationHelper } from './DNSConfigurationHelper'

installBrowserWindow()
afterEach(cleanup)

const configured = {
  external_url: 'https://console.example.com',
  preview_domain: 'apps.example.com',
}

function renderHelper(
  access_mode: 'direct' | 'local',
  settings: Partial<typeof configured>
) {
  mockApi({})
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  client.setQueryData(['platform-settings'], settings)
  return render(
    <QueryClientProvider client={client}>
      <PlatformAccessContext.Provider
        value={{
          accessInfo: {
            access_mode,
            public_ip: '203.0.113.10',
            can_create_domains: true,
          },
          isLoading: false,
          error: null,
          refetch: () => {},
          isLocal: access_mode === 'local',
          isNat: false,
          isCloudflare: false,
          isDirect: access_mode === 'direct',
        }}
      >
        <DNSConfigurationHelper />
      </PlatformAccessContext.Provider>
    </QueryClientProvider>
  )
}

test('a configured platform still shows where to point a new domain', () => {
  const view = renderHelper('direct', configured)
  expect(view.getByText('DNS Target IP Address')).toBeTruthy()
  expect(view.getByText('203.0.113.10')).toBeTruthy()
})

test('opened locally, a platform with a public URL is not called local only', () => {
  const view = renderHelper('local', configured)
  expect(view.queryByText('Local Development Mode')).toBeNull()
  expect(view.getByText(configured.external_url)).toBeTruthy()
  expect(view.getByText('203.0.113.10')).toBeTruthy()
})

test('with no public URL, local mode still explains what is missing', () => {
  const view = renderHelper('local', {})
  expect(view.getByText('Local Development Mode')).toBeTruthy()
})
