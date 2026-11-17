// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import { AuthContext } from '@/contexts/AuthContext-shared'
import { SandboxRuntimeSettings } from './SandboxRuntimeSettings'

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
    ai_config: {
      config_repo: 'acme/global-config',
      config_repo_branch: 'release',
      config_repo_allowlist: ['acme/shared-config', 'platform/*'],
    },
  })
  const auth = {
    user: { id: 1, role: 'admin' },
    isLoading: false,
    error: null,
    logout: async () => {},
    refetch: () => {},
  } as unknown as React.ContextType<typeof AuthContext>

  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <AuthContext.Provider value={auth}>
        <MemoryRouter>
          <SandboxRuntimeSettings />
        </MemoryRouter>
      </AuthContext.Provider>
    </QueryClientProvider>
  )

  expect(html).toContain('acme/shared-config')
  expect(html).toContain('platform/*')
  expect(html).toContain('acme/global-config')
})
