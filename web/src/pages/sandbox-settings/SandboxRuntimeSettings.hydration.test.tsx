// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import {
  cleanup,
  fireEvent,
  render,
  waitFor,
  within,
} from '@testing-library/react'
import { installBrowserWindow } from '@/test/dom'
import { jsonResponse, mockApi } from '@/test/fetch-mock'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { getSandboxResourceDefaultsOptions } from '@/api/client/@tanstack/react-query.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { SandboxRuntimeSettings } from './SandboxRuntimeSettings'

installBrowserWindow()
afterEach(cleanup)

/**
 * Settings are often already cached when this page mounts (another settings
 * page read them). The form must be filled from that cached value, or the
 * next save sends the empty defaults back and overwrites the stored ones.
 */
test('settings already in the cache fill the form on the first render', () => {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(['platform-settings'], {
    agent_sandbox: { sandbox_backend: 'firecracker' },
    ai_config: {
      config_repo: 'acme/global-config',
      config_repo_branch: 'release',
      config_repo_allowlist: ['acme/shared-config', 'platform/*'],
    },
  })
  client.setQueryData(
    getSandboxResourceDefaultsOptions({ query: { backend: 'firecracker' } })
      .queryKey,
    {
      cpu: 0.1,
      max_cpu: 0.1,
      host_cpu_cores: 2,
      memory_mb: 256,
      max_memory_mb: 256,
      host_memory_mb: 2048,
      pids: 256,
      max_pids: 4096,
      timeout_secs: 3600,
      agent_cpu: 2,
      agent_memory_mb: 1024,
      max_timeout_secs: 86400,
    }
  )
  const html = renderToStaticMarkup(settingsTree(client))

  expect(html).toContain('acme/shared-config')
  expect(html).toContain('platform/*')
  expect(html).toContain('Default: 2 cores (host has 2) · Max: 2 cores')
  expect(html).toContain('Default: 1024 MB (host has 2048) · Max: 2048 MB')
  expect(html).toContain('acme/global-config')
})

function settingsTree(client: QueryClient) {
  const auth = {
    user: { id: 1, role: 'admin' },
    isLoading: false,
    error: null,
    logout: async () => {},
    refetch: () => {},
  } as unknown as React.ContextType<typeof AuthContext>

  return (
    <QueryClientProvider client={client}>
      <AuthContext.Provider value={auth}>
        <MemoryRouter>
          <SandboxRuntimeSettings />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )
}

test('retrying a failed placement lookup preserves edits and refreshes the limits', async () => {
  let attempts = 0
  const api = mockApi({
    'GET /v1/sandboxes/resource-defaults': () => {
      attempts += 1
      return attempts === 1
        ? jsonResponse(
            {
              title: 'Placement unavailable',
              status: 503,
              detail: 'Docker capacity is unavailable.',
            },
            503
          )
        : jsonResponse({
            host_cpu_cores: 2,
            host_memory_mb: 2048,
            agent_cpu: 1,
            agent_memory_mb: 1024,
          })
    },
  })
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(['platform-settings'], {
    agent_sandbox: {
      sandbox_backend: 'docker',
      cpu_limit: 1,
      memory_limit_mb: 1024,
    },
  })
  client.setQueryData(['sandbox-status'], null)
  try {
    const view = render(settingsTree(client))
    const placementError = await view.findByText(
      'Sandbox placement limits unavailable'
    )
    const alert = placementError.closest('[role="alert"]')
    if (!(alert instanceof HTMLElement))
      throw new Error('Missing placement alert')
    expect(view.getByText(/Docker capacity is unavailable/)).toBeTruthy()
    const field = view.getByLabelText('Memory (MB)') as HTMLInputElement
    fireEvent.change(field, { target: { value: '1536' } })
    fireEvent.click(within(alert).getByRole('button', { name: 'Retry' }))
    await waitFor(() =>
      expect(
        view.queryByText('Sandbox placement limits unavailable')
      ).toBeNull()
    )
    expect(view.getByText(/Default: 1 cores/)).toBeTruthy()
    expect(attempts).toBe(2)
    expect(field.value).toBe('1536')
  } finally {
    api.restore()
  }
})
