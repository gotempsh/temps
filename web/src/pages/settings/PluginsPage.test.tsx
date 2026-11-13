// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, test } from 'bun:test'
import { MemoryRouter } from 'react-router'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { getPlatformFeaturesQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type { PlatformFeatures } from '@/api/client/types.gen'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { BreadcrumbProvider } from '@/contexts/BreadcrumbContext'
import {
  PLUGIN_REPORTING_QUERY_KEY,
  PLUGINS_QUERY_KEY,
} from '@/hooks/usePlugins'
import { PluginsPage } from './PluginsPage'

// Real providers and a seeded query cache, never `mock.module`: a module mock
// lasts for the whole `bun test` process, so a fake `useAuth` or `usePlugins`
// here would reach every test file that runs after this one.
let role = 'reader'
let pluginsSupported = true
let client: QueryClient

function renderPage() {
  client = new QueryClient({
    defaultOptions: { queries: { retry: false, staleTime: Infinity } },
  })
  client.setQueryData(['repository-plugin-catalog'], {
    available: true,
    source:
      'https://raw.githubusercontent.com/gotempsh/plugins/main/registry/catalog.json',
    platform: 'linux-amd64-gnu',
    plugins: [],
  })
  client.setQueryData(getPlatformFeaturesQueryKey(), {
    external_plugins: pluginsSupported,
  } as PlatformFeatures)
  client.setQueryData(PLUGINS_QUERY_KEY, [])
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter>
        <AuthContext.Provider
          value={{
            user: {
              id: 1,
              name: 'Example User',
              username: 'example',
              avatar_url: '',
              mfa_enabled: false,
              role,
            },
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch: () => {},
          }}
        >
          <BreadcrumbProvider>
            <PluginsPage />
          </BreadcrumbProvider>
        </AuthContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  )
}

/** Whether the page asked for the installation-reporting setting. */
const readReporting = () =>
  client.getQueryCache().find({ queryKey: PLUGIN_REPORTING_QUERY_KEY }) !==
  undefined

describe('PluginsPage management permissions', () => {
  test('explains durable storage requirements while keeping plugins discoverable', () => {
    role = 'admin'
    pluginsSupported = false
    const markup = renderPage()
    expect(markup).toContain('Plugins need persistent storage')
    expect(markup).toContain('Available plugins')
    expect(markup).toContain('full-profile')
  })

  beforeEach(() => {
    pluginsSupported = true
  })

  test('lets readers browse the GitHub catalog without management controls', () => {
    role = 'reader'

    const markup = renderPage()

    expect(readReporting()).toBe(false)
    expect(markup).toContain('Browse')
    expect(markup).toContain('system administrator')
    expect(markup).not.toContain('Reload Plugins')
    expect(markup).toContain('Available plugins')
    expect(markup).not.toContain('Build and install')
    expect(markup).not.toContain('GitHub repository')
    expect(markup).not.toContain('Share installation counts')
  })

  test('enables the catalog and management controls for system administrators', () => {
    role = 'admin'

    const markup = renderPage()

    expect(readReporting()).toBe(false)
    expect(markup).not.toContain('Reload plugins')
    expect(markup).toContain('Available plugins')
    expect(markup).toContain('Install from GitHub')
    expect(markup).toContain('aria-selected="true"')
    expect(markup).not.toContain('Build and install')
    expect(markup).not.toContain('id="plugin-repo"')
    expect(markup).not.toContain('Share installation counts')
    expect(markup).not.toContain('running-plugins-title')
  })

  test('explains when no catalog plugins support the server platform', () => {
    role = 'admin'

    const markup = renderPage()

    expect(markup).toContain(
      'No catalog plugins support this server’s platform yet.'
    )
    expect(markup).toContain(
      'Compatible plugins will appear here when they are listed.'
    )
    expect(markup).not.toContain('No matching plugins.')
    expect(markup).not.toContain('Review and install')
  })
})
