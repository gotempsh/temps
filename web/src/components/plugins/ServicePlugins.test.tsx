// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type {
  EnvironmentResponse,
  PluginServicesResponse,
  ServicePluginResponse,
} from '../../api/client/types.gen'
import {
  addServiceFailure,
  pluginTierSummary,
  servicePluginPath,
  servicePluginStoragePath,
  servicePluginSummary,
  servicePluginsNeedingAttention,
} from '../../lib/plugin-services'
import {
  EnvironmentChoice,
  PluginOwnStorage,
  PluginTierBadge,
  RetiredPluginNotice,
  ServicePluginNotice,
  ServicePluginOrigin,
  ServicePluginStatus,
} from './ServicePlugins'

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
  ...overrides,
})

const services = (
  overrides: Partial<PluginServicesResponse>
): PluginServicesResponse => ({
  configured: true,
  plugins: [],
  retired: [],
  ...overrides,
})

const render = (node: React.ReactNode) =>
  renderToStaticMarkup(<MemoryRouter>{node}</MemoryRouter>)

test('each state of a service plugin has its own tone, label and explanation', () => {
  expect(servicePluginSummary(plugin({}))).toMatchObject({
    tone: 'ok',
    label: 'Running',
  })
  expect(servicePluginSummary(plugin({ state: 'asleep' }))).toMatchObject({
    tone: 'idle',
    label: 'Stopped while idle',
  })
  expect(servicePluginSummary(plugin({ state: 'asleep' })).detail).toContain(
    'starts again on the next request'
  )
  const broken = servicePluginSummary(
    plugin({ state: 'unavailable', detail: "Service 'Reports' did not answer" })
  )
  expect(broken).toMatchObject({ tone: 'error', label: 'Unavailable' })
  expect(broken.detail).toBe("Service 'Reports' did not answer")
})

test('a stopped plugin is not a problem; an unreachable one is', () => {
  const listed = [
    plugin({ name: 'running' }),
    plugin({ name: 'idle', state: 'asleep' }),
    plugin({ name: 'broken', state: 'unavailable' }),
  ]
  expect(
    servicePluginsNeedingAttention(listed).map((entry) => entry.name)
  ).toEqual(['broken'])
})

test('the row status says why a plugin is not running', () => {
  const markup = render(
    <ServicePluginStatus
      plugin={plugin({ state: 'unavailable', detail: 'No running container' })}
    />
  )
  expect(markup).toContain('Unavailable')
  expect(markup).toContain('No running container')
})

test('a plugin links to the service it runs as, or says the service is gone', () => {
  expect(servicePluginPath(plugin({}))).toBe('/projects/reports')
  const linked = render(
    <ServicePluginOrigin plugin={plugin({ stops_when_idle: true })} />
  )
  expect(linked).toContain('href="/projects/reports"')
  expect(linked).toContain('production')
  expect(linked).toContain('stops when idle')

  const orphan = plugin({
    service_name: undefined,
    service_slug: undefined,
    project_id: undefined,
  })
  expect(servicePluginPath(orphan)).toBeUndefined()
  expect(render(<ServicePluginOrigin plugin={orphan} />)).toContain(
    'was deleted'
  )
})

test('the notice lists only unreachable plugins, with the reason and a way out', () => {
  const quiet = render(
    <ServicePluginNotice
      services={services({
        plugins: [plugin({}), plugin({ name: 'idle', state: 'asleep' })],
      })}
      unavailable={false}
      onRemove={() => {}}
    />
  )
  expect(quiet).toBe('')

  const markup = render(
    <ServicePluginNotice
      services={services({
        plugins: [
          plugin({
            name: 'reports',
            state: 'unavailable',
            detail: "Service 'Reports' did not answer",
          }),
        ],
      })}
      unavailable={false}
      onRemove={() => {}}
    />
  )
  expect(markup).toContain('1 plugin is not connected to its service')
  expect(markup).toContain('did not answer')
  expect(markup).toContain('Open service')
  expect(markup).toContain('Remove plugin')
})

test('a failed read of service plugins is said, not shown as an empty list', () => {
  const markup = render(
    <ServicePluginNotice
      services={undefined}
      unavailable={true}
      onRemove={() => {}}
    />
  )
  expect(markup).toContain('Service plugins could not be read')
})

test('the environment choice appears once a service is chosen and marks a stopped one', () => {
  const environment = (
    overrides: Partial<EnvironmentResponse>
  ): EnvironmentResponse =>
    ({
      id: 1,
      name: 'production',
      sleeping: false,
      ...overrides,
    }) as EnvironmentResponse
  const hidden = render(
    <EnvironmentChoice
      chosen={false}
      loading={false}
      failed={false}
      environments={[]}
      value={0}
      onChange={() => {}}
    />
  )
  expect(hidden).toBe('')

  const listed = render(
    <EnvironmentChoice
      chosen
      loading={false}
      failed={false}
      environments={[
        environment({}),
        environment({ id: 2, name: 'preview', sleeping: true }),
      ]}
      value={1}
      onChange={() => {}}
      error="Choose the environment whose deployment is the plugin."
    />
  )
  expect(listed).toContain('production')
  expect(listed).toContain('stopped while idle')
  expect(listed).toContain('role="alert"')

  const empty = render(
    <EnvironmentChoice
      chosen
      loading={false}
      failed={false}
      environments={[]}
      value={0}
      onChange={() => {}}
    />
  )
  expect(empty).toContain('Deploy it first')
})

test("a refusal shows the server's reason", () => {
  expect(
    addServiceFailure({
      title: 'Service Is Not A Plugin',
      detail: "Service 'web' is not a plugin",
    })
  ).toBe("Service 'web' is not a plugin")
  expect(addServiceFailure({ title: 'Service Is Not A Plugin' })).toBe(
    'Service Is Not A Plugin'
  )
  expect(addServiceFailure(new Error('network'))).toBe(
    'The service could not be added as a plugin.'
  )
})

const RETIRED_REASON =
  'This plugin was installed as an executable, and plugins now run only as Temps services.'

test('a plugin installed as an executable is listed with the reason and a way to remove it', () => {
  const markup = render(
    <RetiredPluginNotice
      retired={[
        { name: 'reports', version: '1.2.0', reason: RETIRED_REASON },
        { name: 'audit-export', reason: RETIRED_REASON },
      ]}
      onRemove={() => {}}
    />
  )
  expect(markup).toContain('2 plugins no longer run')
  expect(markup).toContain('reports v1.2.0:')
  // An entry whose version could not be read is still listed.
  expect(markup).toContain('audit-export:')
  expect(markup).toContain('plugins now run only as Temps services')
  expect(markup.match(/Remove entry/g)).toHaveLength(2)
  expect(markup).not.toContain(' disabled=""')
})

test('the removal is not offered while another change is in flight', () => {
  const markup = render(
    <RetiredPluginNotice
      retired={[{ name: 'reports', reason: RETIRED_REASON }]}
      disabled
      onRemove={() => {}}
    />
  )
  expect(markup).toContain('1 plugin no longer runs')
  expect(markup).toContain(' disabled=""')
})

test('nothing is shown when no plugin was installed as an executable', () => {
  expect(render(<RetiredPluginNotice retired={[]} onRemove={() => {}} />)).toBe(
    ''
  )
  expect(
    render(<RetiredPluginNotice retired={undefined} onRemove={() => {}} />)
  ).toBe('')
})

test('a community plugin says what it can never reach', () => {
  expect(pluginTierSummary('community').label).toBe('Community')
  expect(pluginTierSummary('community').detail).toContain(
    "never given Temps's database or data folder"
  )
  expect(pluginTierSummary('verified').label).toBe('Verified')
  const markup = render(<PluginTierBadge plugin={plugin({})} />)
  expect(markup).toContain('Community')
  expect(markup).toContain('Runs in its own container')
})

test("a plugin's own data is the storage linked to its service", () => {
  expect(servicePluginStoragePath(plugin({}))).toBe('/projects/reports/storage')
  expect(
    servicePluginStoragePath(plugin({ service_slug: undefined }))
  ).toBeUndefined()

  const none = render(<PluginOwnStorage plugin={plugin({})} />)
  expect(none).toContain('No database of its own')
  expect(none).toContain('href="/projects/reports/storage"')
  expect(none).toContain('Link one')

  const linked = render(
    <PluginOwnStorage
      plugin={plugin({ own_storage: ['reports-db', 'reports-cache'] })}
    />
  )
  expect(linked).toContain('Own data: reports-db, reports-cache')
  expect(linked).toContain('Manage')

  // A plugin whose service was deleted has nowhere to link storage.
  expect(
    render(<PluginOwnStorage plugin={plugin({ service_slug: undefined })} />)
  ).toBe('')
})
