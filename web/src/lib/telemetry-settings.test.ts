// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
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

  it('is locked off with instructions when the environment forces it', () => {
    // Even an admin cannot override the host-level kill switch, and the
    // switch must not claim telemetry is on.
    const toggle = telemetryToggleState(
      { ...base, enabled: false, source: 'environment' },
      false
    )
    expect(toggle.checked).toBe(false)
    expect(toggle.disabled).toBe(true)
    expect(toggle.disabledReason).toContain('Remove TEMPS_TELEMETRY')
  })

  it('is read-only for users who cannot manage it', () => {
    const toggle = telemetryToggleState({ ...base, can_manage: false }, false)
    expect(toggle.checked).toBe(true)
    expect(toggle.disabled).toBe(true)
    expect(toggle.disabledReason).toContain('instance admins')
  })

  it('is disabled when the reporter is unavailable', () => {
    const toggle = telemetryToggleState(
      { ...base, enabled: false, source: 'unavailable' },
      false
    )
    expect(toggle.disabled).toBe(true)
    expect(toggle.checked).toBe(false)
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
