// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, cleanup, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { getSandboxResourceDefaultsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
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
