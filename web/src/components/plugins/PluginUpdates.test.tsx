// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type {
  AccessDifference,
  PluginServicesResponse,
  ServicePluginResponse,
} from '../../api/client/types.gen'
import {
  pluginUpdateRequests,
  pluginUpdateVersions,
  servicePluginSummary,
  servicePluginsNeedingAttention,
  servicePluginsWithUpdate,
} from '../../lib/plugin-services'
import {
  PluginPinnedBuild,
  PluginUpdateDifference,
  PluginUpdateNotice,
} from './PluginUpdates'

const difference = (
  overrides: Partial<AccessDifference>
): AccessDifference => ({
  version_from: '1.0.0',
  version_to: '1.1.0',
  added_permissions: [],
  removed_permissions: [],
  added_capabilities: [],
  added_public_paths: [],
  added_events: [],
  asks_for_more: false,
  ...overrides,
})

const more = difference({
  added_permissions: ['api_write'],
  added_capabilities: ['api_write'],
  added_public_paths: ['/export/*'],
  added_events: ['project.deleted'],
  asks_for_more: true,
})

const plugin = (
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

const waiting = (update: AccessDifference): ServicePluginResponse =>
  plugin({
    state: 'update_waiting',
    pending_update: {
      deployment_id: 42,
      review: 'review-42',
      since: '2026-11-28T10:00:00Z',
      difference: update,
    },
  })

const services = (
  plugins: ServicePluginResponse[]
): PluginServicesResponse => ({
  configured: true,
  plugins,
  retired: [],
})

const render = (node: React.ReactNode) =>
  renderToStaticMarkup(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>
  )

test('a waiting update has its own state: not running, and not broken', () => {
  const summary = servicePluginSummary(waiting(difference({})))
  expect(summary).toMatchObject({ tone: 'warn', label: 'Update waiting' })
  expect(summary.detail).toContain('until the update is approved')
  const listed = [plugin({}), waiting(difference({}))]
  expect(servicePluginsWithUpdate(listed)).toHaveLength(1)
  // It is listed for review, not as a plugin Temps cannot reach.
  expect(servicePluginsNeedingAttention(listed)).toEqual([])
})

test('what an update asks for is named kind by kind, in the words of the permission screen', () => {
  expect(pluginUpdateRequests(difference({}))).toEqual([])
  expect(pluginUpdateRequests(more)).toEqual([
    {
      title: 'New permissions it asks for',
      items: ['Change things in Temps'],
    },
    {
      title: 'New kinds of API calls it intends to make',
      items: ['Change things through the Temps API'],
    },
    { title: 'New routes open without signing in', items: ['/export/*'] },
    { title: 'New events it listens for', items: ['project.deleted'] },
  ])
})

test('the versions of an update are shown, also when only the build changed', () => {
  expect(pluginUpdateVersions(difference({}))).toBe('v1.0.0 → v1.1.0')
  expect(pluginUpdateVersions(difference({ version_to: '1.0.0' }))).toBe(
    'v1.0.0 (same version, new build)'
  )
  expect(pluginUpdateVersions(difference({ version_from: undefined }))).toBe(
    'v1.1.0'
  )
})

test('a plugin says which deployment it is pinned to and that the next one will wait', () => {
  const pinned = render(<PluginPinnedBuild plugin={plugin({})} />)
  expect(pinned).toContain('Pinned to deployment #41')
  expect(pinned).toContain('a new deployment will wait for approval')

  const held = render(<PluginPinnedBuild plugin={waiting(difference({}))} />)
  expect(held).toContain('deployment #42 waits for approval')

  // A plugin Temps never attached to since pins exist says nothing.
  expect(
    render(
      <PluginPinnedBuild plugin={plugin({ pinned_deployment_id: undefined })} />
    )
  ).toBe('')
})

test('the notice lists the plugins whose update waits and says when one asks for more', () => {
  expect(
    render(
      <PluginUpdateNotice
        services={services([plugin({})])}
        onReview={() => {}}
      />
    )
  ).toBe('')
  expect(
    render(<PluginUpdateNotice services={undefined} onReview={() => {}} />)
  ).toBe('')

  const one = render(
    <PluginUpdateNotice
      services={services([waiting(difference({}))])}
      onReview={() => {}}
    />
  )
  expect(one).toContain('1 plugin update waits for approval')
  expect(one).toContain('disconnected until you approve the new build')
  expect(one).not.toContain('asks for more access')
  expect(one).toContain('Review update')

  const two = render(
    <PluginUpdateNotice
      services={services([
        waiting(more),
        { ...waiting(difference({})), name: 'audit-export' },
      ])}
      disabled
      onReview={() => {}}
    />
  )
  expect(two).toContain('2 plugin updates wait for approval')
  expect(two).toContain('asks for more access')
  expect(two.match(/ disabled=""/g)).toHaveLength(2)
})

test('an update that asks for nothing more is still called new code', () => {
  const markup = render(<PluginUpdateDifference difference={difference({})} />)
  expect(markup).toContain('Asks for no new access')
  expect(markup).toContain('new code that would run with')
  expect(markup).not.toContain('Asks for more than the approved build')
})

test('an update that asks for more lists it and says approving allows none of it', () => {
  const markup = render(
    <PluginUpdateDifference
      difference={{ ...more, removed_permissions: ['events_read'] }}
    />
  )
  expect(markup).toContain('Asks for more than the approved build')
  expect(markup).toContain('Change things in Temps')
  expect(markup).toContain('/export/*')
  expect(markup).toContain('project.deleted')
  expect(markup).toContain('Approving does not allow new permissions')
  expect(markup).toContain('No longer asks for: Receive platform events.')
})
