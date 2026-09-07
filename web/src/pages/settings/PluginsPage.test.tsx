// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { beforeEach, describe, expect, mock, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'

let role = 'reader'
let catalogEnabledValues: boolean[] = []

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
  usePlugins: () => ({ data: [], isLoading: false }),
  usePluginCatalog: (enabled = true) => {
    catalogEnabledValues.push(enabled)
    return {
      data: {
        available: true,
        plugins: [
          {
            author: 'Temps',
            category: 'Observability',
            description: 'Checks deployment health.',
            name: 'deployment-health',
            platforms: {},
            summary: 'Monitor recent deployments.',
            title: 'Deployment Health',
            version: '1.0.0',
          },
        ],
      },
      isLoading: false,
      error: null,
    }
  },
  useInstallPlugin: () => ({
    isPending: false,
    mutateAsync: () => Promise.resolve(),
  }),
  useReloadPlugins: () => ({
    isPending: false,
    mutateAsync: () => Promise.resolve(),
  }),
}))

const { PluginsPage } = await import('./PluginsPage')

describe('PluginsPage management permissions', () => {
  beforeEach(() => {
    catalogEnabledValues = []
  })

  test('keeps plugin management and its catalog request disabled for readers', () => {
    role = 'reader'

    const markup = renderToStaticMarkup(<PluginsPage />)

    expect(catalogEnabledValues).toEqual([false])
    expect(markup).toContain('Verified plugins currently loaded by Temps.')
    expect(markup).toContain('Ask a system administrator to install one.')
    expect(markup).not.toContain('Reload Plugins')
    expect(markup).not.toContain('>Registry<')
    expect(markup).not.toContain('>Install<')
  })

  test('enables the catalog and management controls for system administrators', () => {
    role = 'admin'

    const markup = renderToStaticMarkup(<PluginsPage />)

    expect(catalogEnabledValues).toEqual([true])
    expect(markup).toContain('Reload Plugins')
    expect(markup).toContain('>Registry<')
    expect(markup).toContain('>Install<')
  })
})
