// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, mock, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'

let role = 'reader'
let reportingEnabledValues: boolean[] = []

mock.module('@/contexts/AuthContext', () => ({
  useAuth: () => ({ user: { role } }),
}))

mock.module('@/contexts/BreadcrumbContext', () => ({
  useBreadcrumbs: () => ({ setBreadcrumbs: () => undefined }),
}))

mock.module('@/hooks/usePageTitle', () => ({
  usePageTitle: () => undefined,
}))

mock.module('@/hooks/useSensitiveActionVerification', () => ({
  useSensitiveActionVerification: () => ({
    handleSensitiveActionError: () => false,
    verificationDialog: null,
  }),
}))

mock.module('@/hooks/usePlugins', () => ({
  PLUGINS_QUERY_KEY: ['external-plugins'],
  usePlugins: () => ({ data: [], isLoading: false }),
  usePluginInstallationReporting: (enabled = true) => {
    reportingEnabledValues.push(enabled)
    return {
      data: { enabled: false },
      isLoading: false,
      isError: false,
    }
  },
  useSetPluginInstallationReporting: () => ({
    isPending: false,
    mutateAsync: () => Promise.resolve(),
  }),
  useReloadPlugins: () => ({
    isPending: false,
    mutateAsync: () => Promise.resolve(),
  }),
  useUninstallPlugin: () => ({
    isPending: false,
    mutateAsync: () => Promise.resolve(),
  }),
}))

const { PluginsPage } = await import('./PluginsPage')

function renderPage() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  client.setQueryData(['repository-plugin-catalog'], {
    available: true,
    source:
      'https://raw.githubusercontent.com/gotempsh/plugins/main/registry/catalog.json',
    platform: 'linux-amd64-gnu',
    plugins: [],
  })
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <PluginsPage />
    </QueryClientProvider>
  )
}

describe('PluginsPage management permissions', () => {
  beforeEach(() => {
    reportingEnabledValues = []
  })

  test('lets readers browse the GitHub catalog without management controls', () => {
    role = 'reader'

    const markup = renderPage()

    expect(reportingEnabledValues).toEqual([])
    expect(markup).toContain('Plugins currently loaded by Temps.')
    expect(markup).toContain('Ask a system administrator to install one.')
    expect(markup).not.toContain('Reload Plugins')
    expect(markup).toContain('Available plugins')
    expect(markup).not.toContain('Build and install')
    expect(markup).not.toContain('GitHub repository')
    expect(markup).not.toContain('Share installation counts')
  })

  test('enables the catalog and management controls for system administrators', () => {
    role = 'admin'

    const markup = renderPage()

    expect(reportingEnabledValues).toEqual([])
    expect(markup).toContain('Reload Plugins')
    expect(markup).toContain('Available plugins')
    expect(markup).toContain('Advanced')
    expect(markup).toContain('aria-expanded="false"')
    expect(markup).not.toContain('Build and install')
    expect(markup).not.toContain('id="plugin-repo"')
    expect(markup).not.toContain('Share installation counts')
    expect(markup.indexOf('Available plugins')).toBeLessThan(
      markup.indexOf('running-plugins-title')
    )
  })
})
