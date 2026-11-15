// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { MonitorResponse, RuntimeHealthSummary } from '@/api/client'
import {
  formatRuntimePercent,
  runtimeHealthExplanation,
  runtimeStateTone,
  visibleExternalMonitors,
} from './runtime-health-display'

function monitor(
  id: number,
  managed: boolean,
  environment: number | null
): MonitorResponse {
  return {
    id,
    is_managed: managed,
    environment_id: environment,
    project_id: 1,
    name: 'Monitor',
    monitor_type: 'http',
    monitor_url: '',
    check_interval_seconds: 60,
    is_active: true,
    created_at: '',
    updated_at: '',
  }
}
function health(reason: string): RuntimeHealthSummary {
  return {
    environment_id: 1,
    configured: true,
    state: 'unknown',
    reason,
    portless: true,
    probe: null,
    policy: { enabled: true, paused: false, required_healthy: null },
    coverage: {
      healthy_seconds: 0,
      unhealthy_seconds: 0,
      unknown_seconds: 60,
      excluded_seconds: 0,
      availability_percent: null,
      coverage_percent: 0,
    },
    replicas: [],
    history: [],
  }
}

describe('runtime health display', () => {
  test('unknown availability is a dash; observed failure remains zero percent', () => {
    expect(formatRuntimePercent(null)).toBe('—')
    expect(formatRuntimePercent(undefined)).toBe('—')
    expect(formatRuntimePercent(0)).toBe('0.0%')
    expect(formatRuntimePercent(100)).toBe('100.0%')
    expect(runtimeStateTone('unknown')).toBe('idle')
    expect(runtimeStateTone('unhealthy')).toBe('error')
    expect(runtimeStateTone('healthy')).toBe('ok')
  })
  test('only managed HTTP monitors for confirmed worker environments are withdrawn', () => {
    const monitors = [
      monitor(1, true, 1),
      monitor(2, false, 1),
      monitor(3, true, 2),
      monitor(4, true, null),
    ]
    expect(
      visibleExternalMonitors(monitors, new Set([1])).map((item) => item.id)
    ).toEqual([2, 3, 4])
    expect(visibleExternalMonitors(monitors, new Set()).length).toBe(4)
  })
  test('missing legacy metadata does not hide an independent monitor', () => {
    const legacy = monitor(1, false, 1)
    expect(visibleExternalMonitors([legacy], new Set([1]))).toEqual([legacy])
  })
  test('exec capability and paused time have concrete explanations', () => {
    expect(
      runtimeHealthExplanation(health('runtime_exec_unavailable'))
    ).toContain('without stopping the application')
    expect(runtimeHealthExplanation(health('paused'))).toContain(
      'excluded from uptime'
    )
    expect(runtimeHealthExplanation(health('stale'))).toContain(
      'not counted as an outage or a success'
    )
    const unconfigured = { ...health('not_configured'), configured: false }
    expect(runtimeHealthExplanation(unconfigured)).toContain('Redeploy')
  })
})
