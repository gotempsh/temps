// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test'
import { MemoryRouter } from 'react-router'
import { renderToStaticMarkup } from 'react-dom/server'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type {
  PluginServicesResponse,
  ServicePluginResponse,
} from '@/api/client/types.gen'
import type { PluginManifest } from '@/types/plugins'

// `mock.module` lasts for the whole `bun test` process, so every module
// replaced below is put back after this file: otherwise each test file that
// runs later gets this file's fake user and plugins.
const MOCKED_MODULES = [
  '@/contexts/AuthContext',
  '@/contexts/BreadcrumbContext',
  '@/hooks/usePageTitle',
  '@/hooks/useSensitiveActionVerification',
  '@/hooks/usePlugins',
] as const
const realModules = new Map<string, Record<string, unknown>>()
for (const path of MOCKED_MODULES) {
  realModules.set(path, { ...(await import(path)) })
}
afterAll(() => {
  for (const [path, real] of realModules) mock.module(path, () => real)
})

const RETIRED_REASON =
  'This plugin was installed as an executable, and plugins now run only as Temps services.'

let role = 'reader'
let servicesEnabledValues: boolean[] = []
let added: PluginManifest[] = []
let services: PluginServicesResponse = {
  configured: true,
  plugins: [],
  retired: [],
}

mock.module('@/contexts/AuthContext', () => ({
  useAuth: () => ({ user: { id: 7, role } }),
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
  PLUGIN_SERVICES_QUERY_KEY: ['external-plugins', 'services'],
  usePlugins: () => ({ data: added, isLoading: false }),
  usePluginServices: (enabled = true) => {
    servicesEnabledValues.push(enabled)
    return { data: enabled ? services : undefined, isError: false }
  },
  useAddServicePlugin: () => ({
    isPending: false,
    isError: false,
    reset: () => undefined,
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
  useDisablePlugin: () => ({
    isPending: false,
    mutateAsync: () => Promise.resolve({ message: 'disabled' }),
  }),
  useEnablePlugin: () => ({
    isPending: false,
    variables: undefined,
    mutateAsync: () => Promise.resolve({ message: 'enabled' }),
  }),
}))

const { PluginsPage } = await import('./PluginsPage')

const reports: PluginManifest = {
  name: 'reports',
  version: '1.0.0',
  display_name: 'Reports',
  nav: [],
  requires_db: false,
  health_path: '/health',
}

const servicePlugin = (
  overrides: Partial<ServicePluginResponse>
): ServicePluginResponse => ({
  name: 'reports',
  state: 'running',
  stops_when_idle: false,
  project_id: 7,
  environment_id: 9,
  service_name: 'Reports',
  service_slug: 'reports',
  environment_name: 'production',
  tier: 'community',
  own_storage: [],
  pinned_deployment_id: 41,
  ...overrides,
})

function renderPage(path = '/settings/plugins') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  client.setQueryData(['repository-plugin-catalog'], {
    available: true,
    source:
      'https://raw.githubusercontent.com/gotempsh/plugins/main/registry/catalog.json',
    plugins: [],
  })
  return renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[path]}>
        <PluginsPage />
      </MemoryRouter>
    </QueryClientProvider>
  )
}

describe('PluginsPage', () => {
  beforeEach(() => {
    servicesEnabledValues = []
    services = { configured: true, plugins: [], retired: [] }
    added = []
  })

  test('readers browse the catalogue without management controls', () => {
    role = 'reader'

    const markup = renderPage()

    expect(markup).toContain('Browse')
    expect(markup).toContain('Available plugins')
    expect(markup).not.toContain('Add from service</button>')
    expect(markup).not.toContain('Reload plugins')
    // Which services are plugins is an administrator's view: a reader must
    // not even ask for it.
    expect(servicesEnabledValues.every((enabled) => !enabled)).toBe(true)
  })

  test('adding a service is the only way an administrator adds a plugin', () => {
    role = 'admin'

    const markup = renderPage()

    expect(markup).toContain('Add from service</button>')
    expect(markup).toContain('Available plugins')
    expect(markup).toContain('aria-selected="true"')
    for (const removed of [
      'Install from file',
      'Install from GitHub',
      'Build and install',
      'Review and install',
      'Update plugin',
      'persistent storage',
      'without a container',
    ]) {
      expect(markup).not.toContain(removed)
    }
  })

  test('a plugin installed as an executable is explained on every tab until it is removed', () => {
    role = 'admin'
    services = {
      configured: true,
      plugins: [],
      retired: [{ name: 'reports', version: '1.2.0', reason: RETIRED_REASON }],
    }

    for (const path of ['/settings/plugins', '/settings/plugins?tab=running']) {
      const markup = renderPage(path)
      expect(markup).toContain('1 plugin no longer runs')
      expect(markup).toContain('reports v1.2.0:')
      expect(markup).toContain('plugins now run only as Temps services')
      expect(markup).toContain('Remove entry')
    }
  })

  test('a reader is not shown plugins that no longer run', () => {
    role = 'reader'
    services = {
      configured: true,
      plugins: [],
      retired: [{ name: 'reports', reason: RETIRED_REASON }],
    }

    expect(renderPage()).not.toContain('no longer run')
  })

  test('a disabled plugin stays listed, says so and offers Enable in its row', () => {
    role = 'admin'
    added = [reports]
    services = {
      configured: true,
      plugins: [
        servicePlugin({
          state: 'disabled',
          disabled_at: '2026-11-28T10:00:00Z',
          disabled_by: 7,
          resource_limits: { deployment_id: 41, memory_limit_mb: 512 },
        }),
      ],
      retired: [],
    }

    const markup = renderPage('/settings/plugins?tab=running')

    expect(markup).toContain('Disabled')
    expect(markup).toContain('Disabled by you on ')
    expect(markup).toContain('Enable</button>')
    expect(markup).toContain('Limits: Memory 512 MB · CPU not capped')
  })

  test('a running plugin shows its limits and no Enable button', () => {
    role = 'admin'
    added = [reports]
    services = {
      configured: true,
      plugins: [servicePlugin({ resource_limits: { deployment_id: 41 } })],
      retired: [],
    }

    const markup = renderPage('/settings/plugins?tab=running')

    expect(markup).toContain('it can use all the memory of this server')
    expect(markup).not.toContain('Enable</button>')
    expect(markup).not.toContain('Disabled by')
  })

  test('the list of added plugins says how to add the first one', () => {
    role = 'admin'

    const markup = renderPage('/settings/plugins?tab=running')

    expect(markup).toContain('No plugins are added.')
    expect(markup).toContain(
      'Deploy a plugin as a service, then add that service here.'
    )
    expect(markup).toContain('Reload plugins')
  })
})
