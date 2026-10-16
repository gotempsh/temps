// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { getCiBuilderOptions } from '@/api/client/@tanstack/react-query.gen'
import { CiBuilderPanel } from './CiBuilderPanel'

function client() {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, retryOnMount: false, staleTime: Infinity },
    },
  })
}

test('an unconfigured CI builder stays visible and explains how to provision it', () => {
  const queryClient = client()
  queryClient.setQueryData(getCiBuilderOptions().queryKey, {
    configured: false,
    settings: null,
    reason: 'No CI builder is configured.',
    setup_path: '/settings/nodes#ci-builder',
  })
  const html = renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <CiBuilderPanel />
    </QueryClientProvider>
  )
  for (const text of [
    'No CI builder is configured.',
    'Repository owner',
    'Save CI builder',
    'Provision builder repository',
    'Runner archive SHA-256',
    'Exact OIDC subject',
  ]) {
    expect(html).toContain(text)
  }
  expect(html).toContain('aria-disabled="true"')
  queryClient.clear()
})

test('a settings read failure shows the server reason and retry action', () => {
  const queryClient = client()
  queryClient
    .getQueryCache()
    .build(queryClient, { queryKey: getCiBuilderOptions().queryKey })
    .setState({
      status: 'error',
      fetchStatus: 'idle',
      error: { detail: 'Settings store is unavailable' } as unknown as Error,
    })
  const html = renderToStaticMarkup(
    <QueryClientProvider client={queryClient}>
      <CiBuilderPanel />
    </QueryClientProvider>
  )
  expect(html).toContain('Settings store is unavailable')
  expect(html).toContain('Retry')
  expect(html).not.toContain('No CI builder is configured')
  queryClient.clear()
})
