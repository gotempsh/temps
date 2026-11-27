// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import { emptyPluginGrants, type PluginGrantsValues } from '@/lib/plugin-grants'
import {
  PluginReach,
  PluginWriteScopeFields,
  type PluginReachInfo,
} from './PluginWriteScope'

const writing = (
  overrides: Partial<PluginGrantsValues>
): PluginGrantsValues => ({
  ...emptyPluginGrants(),
  permissions: ['api_write'],
  ...overrides,
})

const services = [
  { id: 7, name: 'storefront' },
  { id: 9, name: 'reports' },
]

const scope = (
  value: PluginGrantsValues,
  props: Partial<Parameters<typeof PluginWriteScopeFields>[0]> = {}
) =>
  renderToStaticMarkup(
    <PluginWriteScopeFields
      value={value}
      onChange={() => {}}
      services={services}
      {...props}
    />
  )

test('with no area chosen the plugin is told to change nothing, and that is said', () => {
  const markup = scope(writing({}))
  expect(markup).toContain('every change this plugin attempts is refused')
  expect(markup.match(/role="checkbox"/g)).toHaveLength(3)
  expect(markup).not.toContain('role="checkbox" aria-checked="true"')
  for (const area of ['Services', 'Environments', 'Deployments'])
    expect(markup).toContain(area)
})

test('a chosen area is checked and the warning is gone', () => {
  const markup = scope(writing({ write_areas: ['deployments'] }))
  // The area itself; "Every service" is a radio, checked by default.
  expect(markup.match(/role="checkbox" aria-checked="true"/g)).toHaveLength(1)
  expect(markup).toContain('role="radio" aria-checked="true"')
  expect(markup).not.toContain('every change this plugin attempts is refused')
  // Every service by default: no service is listed to choose from.
  expect(markup).not.toContain('storefront')
})

test('a limit to some services lists them and says new services cannot be created', () => {
  const markup = scope(
    writing({ write_areas: ['deployments'], write_project_ids: [9] })
  )
  expect(markup).toContain('storefront')
  expect(markup).toContain('reports')
  expect(markup).toContain('cannot create new services')
})

test('a chosen service that is not on the list is still shown so it can be removed', () => {
  const markup = scope(writing({ write_project_ids: [42] }))
  expect(markup).toContain('Service #42')
})

test('loading, failing and a long list each explain themselves', () => {
  const limited = writing({ write_project_ids: [] })
  expect(scope(limited, { servicesLoading: true })).toContain(
    'aria-label="Loading services"'
  )
  const failed = scope(limited, { servicesFailed: true })
  expect(failed).toContain('role="alert"')
  expect(failed).toContain('Services could not be listed')
  expect(scope(limited, { services: [] })).toContain(
    'There are no services yet'
  )
  expect(scope(limited, { servicesTotal: 250 })).toContain(
    'Showing the first 2 of 250 services'
  )
})

const reach = (overrides: Partial<PluginReachInfo>): PluginReachInfo => ({
  public_paths: [],
  events: [],
  outbound_network: {
    profile: 'open',
    detail:
      'The plugin can connect to anything its server can reach, including the internet.',
    settings_path: '/settings/network-policy',
  },
  ...overrides,
})

const shown = (info: PluginReachInfo) =>
  renderToStaticMarkup(
    <MemoryRouter>
      <PluginReach reach={info} />
    </MemoryRouter>
  )

test('a plugin with nothing public and no events says so instead of showing nothing', () => {
  const markup = shown(reach({}))
  expect(markup).toContain('Every route of this plugin needs a Temps session')
  expect(markup).toContain('Events it listens for')
  expect(markup).toContain('Unrestricted')
  expect(markup).toContain('including the internet')
  expect(markup).toContain('href="/settings/network-policy"')
})

test('public routes, events and a closed network are listed', () => {
  const markup = shown(
    reach({
      public_paths: ['/hooks/*'],
      events: ['deployment.succeeded'],
      outbound_network: {
        profile: 'none',
        detail: 'The plugin cannot connect to anything outside Temps.',
        settings_path: '/settings/network-policy',
      },
    })
  )
  expect(markup).toContain('/hooks/*')
  expect(markup).toContain('Anyone who can reach this server can call these')
  expect(markup).toContain('deployment.succeeded')
  expect(markup).toContain('No outbound access')
  expect(markup).toContain('cannot connect to anything outside Temps')
})
