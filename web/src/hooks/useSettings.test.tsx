// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { getSandboxResourceDefaultsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { isStaleRevision } from '@/lib/stale-revision'
import { useUpdateSettings } from './useSettings'

installBrowserWindow()
afterEach(cleanup)

test('saving sandbox settings invalidates cached limits for every placement', async () => {
  const api = mockApi({
    'GET /settings': () => jsonResponse({ preview_domain: 'example.com' }),
    'PUT /settings': () => jsonResponse({ message: 'Updated' }),
  })
  const client = new QueryClient()
  const keys = ['control-plane', 'worker'].map((node) =>
    getSandboxResourceDefaultsQueryKey({ query: { backend: 'docker', node } })
  )
  for (const key of keys) client.setQueryData(key, { max_cpu: 1 })

  const { result } = renderHook(() => useUpdateSettings(), {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  })
  act(() => {
    result.current.mutate({
      agent_sandbox: {
        default_provider: 'claude_cli',
        providers: {},
        api_key_saved: false,
        auth_type: 'none',
        sandbox_backend: 'docker',
        enabled: true,
        runtime: 'node',
        custom_image: '',
        network_mode: 'none',
        cpu_limit: 2,
        memory_limit_mb: 1024,
      },
    })
  })
  await waitFor(() => expect(result.current.isSuccess).toBe(true))
  expect(api.called('PUT', '/settings')).toBe(true)
  for (const key of keys)
    expect(client.getQueryState(key)?.isInvalidated).toBe(true)
})

test('a save sends the revision the page loaded with, and a stale one is refused', async () => {
  let sent: { expected_revision?: unknown; preview_domain?: unknown } = {}
  mockApi({
    // Someone else saved after this page loaded: the server is at "r2".
    'GET /settings': () =>
      jsonResponse({ preview_domain: 'other.example.com', revision: 'r2' }),
    'PUT /settings': async (request) => {
      sent = await request.json()
      return jsonResponse(
        {
          title: 'Settings Changed Since They Were Read',
          detail: 'Platform settings changed since it was read',
          expected_revision: 'r1',
          current_revision: 'r2',
        },
        409
      )
    },
  })
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  client.setQueryData(['platform-settings'], {
    preview_domain: 'example.com',
    revision: 'r1',
  })
  const { result } = renderHook(() => useUpdateSettings(), {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  })
  act(() => {
    result.current.mutate({ preview_domain: 'mine.example.com' })
  })
  await waitFor(() => expect(result.current.isError).toBe(true))
  expect(sent.expected_revision).toBe('r1')
  expect(sent.preview_domain).toBe('mine.example.com')
  expect(isStaleRevision(result.current.error)).toBe(true)
  expect(
    (client.getQueryData(['platform-settings']) as { preview_domain: string })
      .preview_domain
  ).toBe('example.com')
})

test('a successful save keeps the new revision for the next save', async () => {
  mockApi({
    'GET /settings': () =>
      jsonResponse({ preview_domain: 'example.com', revision: 'r1' }),
    'PUT /settings': () => jsonResponse({ message: 'Updated', revision: 'r2' }),
  })
  const client = new QueryClient()
  client.setQueryData(['platform-settings'], {
    preview_domain: 'example.com',
    revision: 'r1',
  })
  const { result } = renderHook(() => useUpdateSettings(), {
    wrapper: ({ children }: { children: ReactNode }) => (
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    ),
  })
  act(() => {
    result.current.mutate({ preview_domain: 'mine.example.com' })
  })
  await waitFor(() => expect(result.current.isSuccess).toBe(true))
  expect(result.current.data?.revision).toBe('r2')
})
