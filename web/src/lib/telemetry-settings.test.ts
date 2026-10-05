// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { QueryClient } from '@tanstack/react-query'
import { getTelemetrySettingsQueryKey } from '@/api/client/@tanstack/react-query.gen'
import type { TelemetryStatusResponse } from '@/api/client/types.gen'
import {
  cacheSavedTelemetryPreference,
  groupTelemetryEvents,
  summarizeTelemetryState,
  telemetryToggleState,
} from './telemetry-settings'

const base = {
  enabled: true,
  source: 'default' as const,
  env_var: 'TEMPS_TELEMETRY',
  default_enabled: true,
  can_manage: true,
  admin_preference: null,
}

describe('summarizeTelemetryState', () => {
  it('explains the default', () => {
    const summary = summarizeTelemetryState(base)
    expect(summary.tone).toBe('ok')
    expect(summary.label).toBe('Sending')
    expect(summary.reason).toContain('On by default')
  })

  it('names the environment variable when it forces telemetry off', () => {
    const summary = summarizeTelemetryState({
      ...base,
      enabled: false,
      source: 'environment',
    })
    expect(summary.tone).toBe('idle')
    expect(summary.label).toBe('Off')
    expect(summary.reason).toContain('TEMPS_TELEMETRY')
    expect(summary.reason).toContain('overrides')
  })

  it('attributes an admin choice in both directions', () => {
    expect(
      summarizeTelemetryState({
        ...base,
        enabled: false,
        source: 'admin_setting',
      }).reason
    ).toContain('turned telemetry off')
    expect(
      summarizeTelemetryState({ ...base, source: 'admin_setting' }).reason
    ).toContain('turned telemetry on')
  })

  it('reports an unavailable reporter as a warning, not as off', () => {
    const summary = summarizeTelemetryState({
      ...base,
      enabled: false,
      source: 'unavailable',
    })
    expect(summary.tone).toBe('warn')
    expect(summary.label).toBe('Unavailable')
  })
})

describe('telemetryToggleState', () => {
  it('lets an admin change it', () => {
    expect(telemetryToggleState(base, false)).toEqual({
      checked: true,
      disabled: false,
      disabledReason: null,
    })
  })

  it('blocks duplicate submissions while saving', () => {
    expect(telemetryToggleState(base, true).disabled).toBe(true)
  })

  it('can save an opt-out while the host override keeps reporting off', () => {
    const toggle = telemetryToggleState(
      {
        ...base,
        enabled: false,
        source: 'environment',
        admin_preference: false,
      },
      false
    )
    expect(toggle.checked).toBe(false)
    expect(toggle.disabled).toBe(false)
  })

  it('is read-only for users who cannot manage it', () => {
    const toggle = telemetryToggleState({ ...base, can_manage: false }, false)
    expect(toggle.checked).toBe(true)
    expect(toggle.disabled).toBe(true)
    expect(toggle.disabledReason).toContain('instance admins')
  })

  it('can save a preference while the reporter is unavailable', () => {
    expect(
      telemetryToggleState(
        { ...base, enabled: false, source: 'unavailable' },
        false
      ).disabled
    ).toBe(false)
  })
})

describe('groupTelemetryEvents', () => {
  it('groups by category in a stable order and keeps every event', () => {
    const events = [
      { name: 'error_summary', category: 'health' as const },
      { name: 'deploy_succeeded', category: 'deployments' as const },
      { name: 'instance_heartbeat', category: 'instance' as const },
      { name: 'deploy_failed', category: 'deployments' as const },
    ]
    const groups = groupTelemetryEvents(events)
    expect(groups.map((g) => g.category)).toEqual([
      'instance',
      'deployments',
      'health',
    ])
    expect(groups[1].events).toEqual(['deploy_succeeded', 'deploy_failed'])
    expect(groups[1].label).toBe('Deployments')
    expect(groups.flatMap((g) => g.events)).toHaveLength(events.length)
  })

  it('returns nothing for an empty catalog', () => {
    expect(groupTelemetryEvents([])).toEqual([])
  })
})

it('an older status request cannot overwrite an acknowledged saved opt-out', async () => {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  const queryKey = getTelemetrySettingsQueryKey()
  const oldStatus: TelemetryStatusResponse = {
    ...base,
    env_opted_out: false,
    events: [],
    privacy_doc_url: 'https://temps.sh/privacy',
  }
  const saved: TelemetryStatusResponse = {
    ...oldStatus,
    enabled: false,
    admin_preference: false,
    source: 'admin_setting',
  }
  let resolve!: (status: TelemetryStatusResponse) => void
  const request = queryClient
    .fetchQuery({
      queryKey,
      queryFn: () =>
        new Promise<TelemetryStatusResponse>((done) => {
          resolve = done
        }),
    })
    .catch(() => undefined)
  await cacheSavedTelemetryPreference(queryClient, saved)
  resolve(oldStatus)
  await request
  await Promise.resolve()
  expect(
    queryClient.getQueryData<TelemetryStatusResponse>([...queryKey])
  ).toEqual(saved)
  queryClient.clear()
})
