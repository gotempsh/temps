// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'
import type { ServicePluginResponse } from '../../api/client/types.gen'
import {
  pluginLimitsSummary,
  servicePluginSummary,
  servicePluginsNeedingAttention,
} from '../../lib/plugin-services'
import {
  PluginDisabledNote,
  PluginEnableButton,
  PluginResourceLimits,
} from './PluginControls'

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

const disabled = plugin({
  state: 'disabled',
  disabled_at: '2026-11-28T10:00:00Z',
  disabled_by: 7,
})

const render = (node: React.ReactNode) =>
  renderToStaticMarkup(<MemoryRouter>{node}</MemoryRouter>)

test('a disabled plugin has its own state: switched off, not broken', () => {
  const summary = servicePluginSummary(disabled)
  expect(summary.label).toBe('Disabled')
  expect(summary.tone).toBe('idle')
  expect(summary.detail).toContain('until it is enabled')
  // The server's reason wins when it gives one.
  expect(
    servicePluginSummary({ ...disabled, detail: 'Switched off for review.' })
      .detail
  ).toBe('Switched off for review.')
  expect(servicePluginsNeedingAttention([disabled])).toEqual([])
})

test('limits are said in megabytes or gigabytes and in cores', () => {
  expect(
    pluginLimitsSummary({
      deployment_id: 41,
      memory_limit_mb: 512,
      cpu_limit_microcores: 500_000,
    })
  ).toEqual({ memory: 'Memory 512 MB', cpu: 'CPU 0.5 cores', capped: true })
  expect(
    pluginLimitsSummary({
      deployment_id: 41,
      memory_limit_mb: 2048,
      cpu_limit_microcores: 1_000_000,
    })
  ).toEqual({ memory: 'Memory 2 GB', cpu: 'CPU 1 core', capped: true })
  expect(pluginLimitsSummary({ deployment_id: 41 })).toEqual({
    memory: 'Memory not capped',
    cpu: 'CPU not capped',
    capped: false,
  })
})

test('a plugin shows the limits of its container and where to change them', () => {
  const markup = render(
    <PluginResourceLimits
      plugin={plugin({
        resource_limits: { deployment_id: 41, memory_limit_mb: 512 },
      })}
    />
  )
  expect(markup).toContain('Limits: Memory 512 MB · CPU not capped')
  expect(markup).toContain('href="/projects/reports/environments"')
  expect(markup).toContain('Change limits')
  expect(markup).not.toContain('all the memory')
})

test('a plugin without a memory limit says what that means and offers to set one', () => {
  const markup = render(
    <PluginResourceLimits
      plugin={plugin({ resource_limits: { deployment_id: 41 } })}
    />
  )
  expect(markup).toContain('Memory not capped')
  expect(markup).toContain('it can use all the memory of this server')
  expect(markup).toContain('Set a limit')
})

test('limits are left out until a deployment is known, and stay without a link when the service is gone', () => {
  expect(render(<PluginResourceLimits plugin={plugin({})} />)).toBe('')
  const orphaned = render(
    <PluginResourceLimits
      plugin={plugin({
        service_slug: null,
        resource_limits: { deployment_id: 41, memory_limit_mb: 512 },
      })}
    />
  )
  expect(orphaned).toContain('Memory 512 MB')
  expect(orphaned).not.toContain('<a ')
})

test('a disabled plugin says who switched it off and what was kept', () => {
  const mine = render(
    <PluginDisabledNote plugin={disabled} currentUserId={7} />
  )
  expect(mine).toContain('Disabled by you on ')
  expect(mine).toContain('Its service keeps running')
  expect(mine).toContain('its permissions are kept')
  const theirs = render(
    <PluginDisabledNote plugin={disabled} currentUserId={8} />
  )
  expect(theirs).toContain('Disabled by another administrator on ')
  // The user who disabled it was deleted: the plugin stays off.
  const unknown = render(
    <PluginDisabledNote
      plugin={{ ...disabled, disabled_by: null }}
      currentUserId={8}
    />
  )
  expect(unknown).toContain('Disabled on ')
  expect(render(<PluginDisabledNote plugin={plugin({})} />)).toBe('')
})

test('only a disabled plugin offers Enable, and it waits while enabling', () => {
  const enable = () => undefined
  expect(
    render(<PluginEnableButton plugin={plugin({})} onEnable={enable} />)
  ).toBe('')
  expect(
    render(<PluginEnableButton plugin={undefined} onEnable={enable} />)
  ).toBe('')
  const ready = render(
    <PluginEnableButton plugin={disabled} onEnable={enable} />
  )
  expect(ready).toContain('Enable</button>')
  expect(ready).not.toContain('disabled=""')
  const pending = render(
    <PluginEnableButton plugin={disabled} pending onEnable={enable} />
  )
  expect(pending).toContain('Enabling…')
  expect(pending).toContain('disabled=""')
})
