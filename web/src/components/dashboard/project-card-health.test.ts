// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  projectHealthIndicator,
  type ProjectHealthInput,
} from './project-card-health'

function summary(
  overrides: Partial<ProjectHealthInput> = {}
): ProjectHealthInput {
  return {
    status: 'healthy',
    total_requests: 1200,
    total_errors: 3,
    error_rate: 0.3,
    avg_response_time_ms: 42.4,
    ...overrides,
  }
}

describe('projectHealthIndicator', () => {
  test('reports a measured healthy project with its numbers', () => {
    const indicator = projectHealthIndicator({ health: summary() })

    expect(indicator.tone).toBe('healthy')
    expect(indicator.label).toBe('Healthy')
    expect(indicator.detail).toContain('1,200 requests')
    expect(indicator.detail).toContain('the last 24 hours')
    expect(indicator.detail).toContain('42 ms')
  })

  test('distinguishes degraded from down', () => {
    expect(
      projectHealthIndicator({
        health: summary({ status: 'degraded', error_rate: 22.5 }),
      })
    ).toMatchObject({ tone: 'degraded', label: 'Degraded' })

    expect(
      projectHealthIndicator({
        health: summary({ status: 'down', error_rate: 91.2 }),
      })
    ).toMatchObject({ tone: 'down', label: 'Down' })
  })

  test('treats the status page wording for healthy as healthy', () => {
    expect(
      projectHealthIndicator({ health: summary({ status: 'operational' }) })
    ).toMatchObject({ tone: 'healthy', label: 'Healthy' })
  })

  test('calls a project with no requests idle, never missing', () => {
    // The regression this guards: the backend reports "unknown" for a project
    // that received nothing, and the card used to render no indicator at all.
    const indicator = projectHealthIndicator({
      health: summary({
        status: 'unknown',
        total_requests: 0,
        total_errors: 0,
        error_rate: 0,
        avg_response_time_ms: 0,
      }),
    })

    expect(indicator.tone).toBe('idle')
    expect(indicator.label).toBe('No traffic')
    expect(indicator.detail).toContain('No user requests reached this project')
  })

  test('never reports an unreachable health service as idle', () => {
    const indicator = projectHealthIndicator({ error: true })

    expect(indicator.tone).toBe('unavailable')
    expect(indicator.label).toBe('Unavailable')
    expect(indicator.detail).toContain('proxy')
  })

  test('shows a pending state only while the first load is in flight', () => {
    expect(projectHealthIndicator({ loading: true })).toMatchObject({
      tone: 'pending',
      label: 'Checking…',
    })

    // A background refetch must not blank out health we already have.
    expect(
      projectHealthIndicator({ loading: true, health: summary() })
    ).toMatchObject({ tone: 'healthy', label: 'Healthy' })
  })

  test('says so when the summary omits the project entirely', () => {
    expect(projectHealthIndicator({})).toMatchObject({
      tone: 'unavailable',
      label: 'No health data',
    })
  })

  test('surfaces an unrecognized status instead of guessing', () => {
    const indicator = projectHealthIndicator({
      health: summary({ status: 'partial_outage' }),
    })

    expect(indicator.tone).toBe('unavailable')
    expect(indicator.label).toBe('Unknown')
    expect(indicator.detail).toContain('partial_outage')
    expect(indicator.detail).toContain('1,200 requests')
  })

  test('reports a healthy monitor for a project with no traffic', () => {
    // The reported bug: bridge-relayer had a production monitor sitting at 100%
    // uptime and still showed "unknown", because proxy health excludes Temps'
    // own monitor checks (is_system_request = FALSE) and no human had visited
    // in the default 1-hour window.
    const indicator = projectHealthIndicator({
      health: summary({
        status: 'unknown',
        total_requests: 0,
        total_errors: 0,
        error_rate: 0,
        avg_response_time_ms: 0,
      }),
      monitor: { status: 'operational' },
    })

    expect(indicator.tone).toBe('healthy')
    expect(indicator.label).toBe('Uptime healthy')
    expect(indicator.detail).toContain('uptime monitor')
  })

  test('qualifies healthy uptime when request traffic is degraded', () => {
    const indicator = projectHealthIndicator({
      health: summary({
        status: 'degraded',
        total_requests: 150,
        total_errors: 20,
        error_rate: 13.3,
      }),
      monitor: { status: 'operational' },
    })

    expect(indicator).toMatchObject({
      tone: 'healthy',
      label: 'Uptime healthy',
    })
    expect(indicator.label).not.toBe('Healthy')
  })

  test('lets the monitor answer when the traffic query failed entirely', () => {
    expect(
      projectHealthIndicator({ error: true, monitor: { status: 'down' } })
    ).toMatchObject({ tone: 'down', label: 'Uptime down' })

    expect(
      projectHealthIndicator({
        loading: true,
        monitor: { status: 'degraded' },
      })
    ).toMatchObject({ tone: 'degraded', label: 'Uptime degraded' })
  })

  test('a failing monitor outranks quiet-but-clean traffic', () => {
    expect(
      projectHealthIndicator({
        health: summary({ status: 'healthy' }),
        monitor: { status: 'down' },
      })
    ).toMatchObject({ tone: 'down', label: 'Uptime down' })
  })

  test('falls back to traffic when the project has no monitors', () => {
    expect(
      projectHealthIndicator({
        health: summary({ status: 'degraded', error_rate: 22.5 }),
        monitor: { status: 'no_monitors' },
      })
    ).toMatchObject({ tone: 'degraded', label: 'Degraded' })

    // No monitor and no traffic: still not "unknown", and it says what to do.
    const idle = projectHealthIndicator({
      health: summary({
        status: 'unknown',
        total_requests: 0,
        total_errors: 0,
        error_rate: 0,
        avg_response_time_ms: 0,
      }),
      monitor: { status: 'no_monitors' },
    })
    expect(idle).toMatchObject({ tone: 'idle', label: 'No traffic' })
    expect(idle.detail).toContain('Add an uptime monitor')
  })

  test('ignores a monitor status it does not recognise', () => {
    expect(
      projectHealthIndicator({
        health: summary({ status: 'healthy' }),
        monitor: { status: 'maintenance' },
      })
    ).toMatchObject({ tone: 'healthy', label: 'Healthy' })
  })

  test('describes a non-default window in the detail text', () => {
    expect(
      projectHealthIndicator({ health: summary(), windowHours: 1 }).detail
    ).toContain('the last hour')

    expect(
      projectHealthIndicator({ health: summary(), windowHours: 6 }).detail
    ).toContain('the last 6 hours')
  })
})
