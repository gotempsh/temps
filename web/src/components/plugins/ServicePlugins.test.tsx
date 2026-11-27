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
import { isolationSummary } from '../../lib/plugin-isolation'
import {
  addServiceFailure,
  servicePluginPath,
  servicePluginSummary,
  servicePluginsNeedingAttention,
} from '../../lib/plugin-services'
import {
  EnvironmentChoice,
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
  ...overrides,
})

const services = (
  overrides: Partial<PluginServicesResponse>
): PluginServicesResponse => ({
  configured: true,
  plugins: [],
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

test('a plugin that runs as a service counts as contained', () => {
  expect(isolationSummary('service')).toMatchObject({
    tone: 'ok',
    label: 'Runs as a service',
  })
})
