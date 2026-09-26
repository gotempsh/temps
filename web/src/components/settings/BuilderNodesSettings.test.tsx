// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { NodeInfoResponse, UserResponse } from '@/api/client'
import { AuthContext } from '@/contexts/AuthContext-shared'
import { getGlobalBuildNodesOptions } from '@/api/client/@tanstack/react-query.gen'
import { BuilderNodesForm, BuilderNodesSettings } from './BuilderNodesSettings'
import { settingsNavigationGroups } from './settings-navigation'

const worker = {
  id: 7,
  name: 'Build worker',
  role: 'worker',
  status: 'offline',
  architecture: 'linux/amd64',
} as NodeInfoResponse

function renderForm(
  overrides: Partial<Parameters<typeof BuilderNodesForm>[0]> = {}
) {
  const props: Parameters<typeof BuilderNodesForm>[0] = {
    policy: { node_ids: [7], effective_node_ids: [7], source: 'global' },
    nodes: [worker],
    rosterKnown: true,
    rosterLoading: false,
    rosterError: false,
    canList: true,
    canEdit: true,
    permissionsPending: false,
    permissionsError: false,
    retryPermissions() {},
    retryRoster() {},
    policyError: false,
    retryPolicy() {},
    saving: false,
    saveError: null,
    onSave: async () => ({ source: 'automatic' }),
    ...overrides,
  }
  return renderToStaticMarkup(
    <MemoryRouter>
      <BuilderNodesForm {...props} />
    </MemoryRouter>
  )
}

test('global navigation and worker management are discoverable', () => {
  expect(
    settingsNavigationGroups
      .flatMap((group) => group.items)
      .some((item) => item.url === '/settings/build-nodes')
  ).toBe(true)
  const html = renderForm()
  expect(html).toContain('Manage worker nodes')
  expect(html).toContain('Automatic selection')
  expect(html).toContain('offline')
  expect(html).toContain('linux/amd64')
  expect(html).toContain('Move Build worker up')
  expect(html).toContain('Remove Build worker')
  expect(html).not.toContain('data-page-container')
})

test('project inheritance shows effective defaults, reset choice and global settings link', () => {
  const html = renderForm({
    projectId: 2,
    policy: { source: 'global', node_ids: null, effective_node_ids: [7] },
  })
  expect(html).toContain('Global default: Build worker')
  expect(html).toContain('Inherit global default')
  expect(html).toContain('href="/settings/build-nodes"')
  // The project shell owns h1; this page and its group continue the hierarchy.
  expect(html).not.toContain('<h1')
  expect(html).toContain('Pipelines</h2>')
  expect(html).toContain('Builder nodes</h3>')
  expect(html.match(/<form\b/g)).toHaveLength(1)
  expect(html.match(/Save changes/g)).toHaveLength(1)
  expect(html).not.toContain('Build placement')
  expect(html).not.toContain('Saved selection')
})

test('removed workers remain visible until explicitly removed', () => {
  const html = renderForm({ nodes: [] })
  expect(html).toContain('Worker #7')
  expect(html).toContain('Missing worker')
  expect(html).toContain('No worker nodes joined')
  expect(html).toContain('Add a worker node')
})

test('node lookup failures retain selection and offer retry instead of empty onboarding', () => {
  const html = renderForm({ rosterKnown: false, rosterError: true, nodes: [] })
  expect(html).toContain('Worker #7')
  expect(html).toContain('Retry worker list')
  expect(html).not.toContain('No worker nodes joined')
  expect(html).toContain('Worker ID')
})

test('read-only permissions disable editing with an explanation', () => {
  const html = renderForm({ canEdit: false })
  expect(html).toContain('Read-only.')
  expect(html).toContain('settings:write')
  expect(html).toContain('<fieldset disabled=""')
  expect(html).toContain('aria-disabled="true"')
})

test('save failures keep the pool and show actionable server detail', () => {
  const html = renderForm({
    saveError: { detail: 'Worker 7 no longer exists. Select another worker.' },
  })
  expect(html).toContain('Builder settings were not saved')
  expect(html).toContain('Worker 7 no longer exists')
  expect(html).toContain('Remove Build worker')
})

test('background policy failure blocks saves and offers retry', () => {
  const html = renderForm({ policyError: true })
  expect(html).toContain('Saving is paused')
  expect(html).toContain('Retry builder settings')
  expect(html).toContain('Global default: Build worker')
})

test('loading and failed policy reads are distinct from automatic selection', () => {
  for (const failed of [false, true]) {
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false, retryOnMount: false } },
    })
    if (failed)
      client
        .getQueryCache()
        .build(client, { queryKey: getGlobalBuildNodesOptions().queryKey })
        .setState({
          status: 'error',
          error: new Error('Settings access denied'),
          fetchStatus: 'idle',
        })
    const html = renderToStaticMarkup(
      <QueryClientProvider client={client}>
        <AuthContext.Provider
          value={{
            user: { role: 'admin' } as UserResponse,
            isLoading: false,
            error: null,
            logout: async () => {},
            refetch() {},
          }}
        >
          <MemoryRouter>
            <BuilderNodesSettings />
          </MemoryRouter>
        </AuthContext.Provider>
      </QueryClientProvider>
    )
    expect(html).toContain(
      failed ? 'Could not load builder settings' : 'Loading builder settings'
    )
    expect(html).not.toContain('Automatic: builds')
    client.clear()
  }
})
