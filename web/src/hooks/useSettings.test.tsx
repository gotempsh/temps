// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from '@tanstack/react-query'
import {
  act,
  cleanup,
  fireEvent,
  renderHook,
  screen,
  waitFor,
} from '@testing-library/react'
import { Toaster, toast } from 'sonner'
import type { ReactNode } from 'react'
import { getSandboxResourceDefaultsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { isStaleRevision } from '@/lib/stale-revision'
import { getPlatformSettings } from '@/api/platformSettings'
import { useUpdateSettings } from './useSettings'

installBrowserWindow()
afterEach(() => {
  cleanup()
  toast.dismiss()
})

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

test('a form that merged a refetch into its edits sends the revision the edits started from', async () => {
  let sent: { expected_revision?: unknown } = {}
  mockApi({
    'GET /settings': () =>
      jsonResponse({ preview_domain: 'other.example.com', revision: 'r2' }),
    'PUT /settings': async (request) => {
      sent = await request.json()
      return jsonResponse({ message: 'Updated', revision: 'r3' })
    },
  })
  const client = new QueryClient()
  // The background refetch already brought someone else's save.
  client.setQueryData(['platform-settings'], {
    preview_domain: 'other.example.com',
    revision: 'r2',
  })
  const { result } = renderHook(
    () => useUpdateSettings({ baseRevision: 'r1' }),
    {
      wrapper: ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={client}>{children}</QueryClientProvider>
      ),
    }
  )
  act(() => {
    result.current.mutate({ preview_domain: 'mine.example.com' })
  })
  await waitFor(() => expect(result.current.isSuccess).toBe(true))
  expect(sent.expected_revision).toBe('r1')
})

test('after a refused save, "Load latest" re-reads the settings in place and hands them to the page', async () => {
  let reads = 0
  mockApi({
    'GET /settings': () => {
      reads += 1
      return jsonResponse({
        preview_domain: 'other.example.com',
        revision: 'r2',
      })
    },
    'PUT /settings': () =>
      jsonResponse(
        {
          title: 'Settings Changed Since They Were Read',
          detail: 'Platform settings changed since it was read',
          expected_revision: 'r1',
          current_revision: 'r2',
        },
        409
      ),
  })
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  client.setQueryData(['platform-settings'], {
    preview_domain: 'example.com',
    revision: 'r1',
  })
  const loaded: string[] = []
  let reloaded = false
  const reload = window.location.reload
  window.location.reload = () => {
    reloaded = true
  }
  const { result } = renderHook(
    () => {
      // The page's read of the settings (fresh: seeded below).
      useQuery({
        queryKey: ['platform-settings'],
        queryFn: getPlatformSettings,
        staleTime: Infinity,
      })
      return useUpdateSettings({
        baseRevision: 'r1',
        onLatestLoaded: (latest) => loaded.push(latest.revision ?? ''),
      })
    },
    {
      wrapper: ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={client}>
          {children}
          <Toaster />
        </QueryClientProvider>
      ),
    }
  )
  act(() => {
    result.current.mutate({ preview_domain: 'mine.example.com' })
  })
  await waitFor(() => expect(result.current.isError).toBe(true))
  const loadLatest = await screen.findByRole('button', { name: 'Load latest' })
  // The save read the settings once itself, before writing.
  const readsBefore = reads
  await act(async () => {
    fireEvent.click(loadLatest)
  })
  await waitFor(() => expect(loaded).toEqual(['r2']))
  expect(reads).toBe(readsBefore + 1)
  expect(reloaded).toBe(false)
  window.location.reload = reload
})

test('a "Load latest" whose read fails keeps the form as it was and offers a retry', async () => {
  let failReads = false
  mockApi({
    'GET /settings': () =>
      failReads
        ? jsonResponse({ detail: 'Settings are unavailable' }, 503)
        : jsonResponse({ preview_domain: 'other.example.com', revision: 'r2' }),
    'PUT /settings': () =>
      jsonResponse(
        {
          title: 'Settings Changed Since They Were Read',
          detail: 'Platform settings changed since it was read',
          expected_revision: 'r1',
          current_revision: 'r2',
        },
        409
      ),
  })
  const client = new QueryClient({
    defaultOptions: { mutations: { retry: false } },
  })
  client.setQueryData(['platform-settings'], {
    preview_domain: 'example.com',
    revision: 'r1',
  })
  const loaded: string[] = []
  const { result } = renderHook(
    () => {
      useQuery({
        queryKey: ['platform-settings'],
        queryFn: getPlatformSettings,
        staleTime: Infinity,
        retry: false,
      })
      return useUpdateSettings({
        baseRevision: 'r1',
        onLatestLoaded: (latest) => loaded.push(latest.revision ?? ''),
      })
    },
    {
      wrapper: ({ children }: { children: ReactNode }) => (
        <QueryClientProvider client={client}>
          {children}
          <Toaster />
        </QueryClientProvider>
      ),
    }
  )
  act(() => {
    result.current.mutate({ preview_domain: 'mine.example.com' })
  })
  await waitFor(() => expect(result.current.isError).toBe(true))
  failReads = true
  await act(async () => {
    fireEvent.click(await screen.findByRole('button', { name: 'Load latest' }))
  })
  expect(
    await screen.findByText('Could not load the latest settings')
  ).toBeTruthy()
  // The cached document is not handed over as if it were the latest.
  expect(loaded).toEqual([])

  failReads = false
  await act(async () => {
    fireEvent.click(await screen.findByRole('button', { name: 'Retry' }))
  })
  await waitFor(() => expect(loaded).toEqual(['r2']))
})
