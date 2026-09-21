// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { workerIngressStatus, type WorkerIngressState } from './worker-ingress'

const now = Date.parse('2026-09-21T12:00:00Z')
const running: WorkerIngressState = {
  status: 'active',
  last_heartbeat: new Date(now).toISOString(),
  public_ingress_enabled: true,
  public_ingress_running: true,
  public_ingress_certificate_count: 1,
}

describe('worker ingress status', () => {
  test('does not present requested configuration as runtime readiness', () => {
    expect(
      workerIngressStatus({ ...running, public_ingress_running: null }, now)
        .label
    ).toBe('Waiting for worker')
    expect(
      workerIngressStatus({ ...running, public_ingress_running: false }, now)
        .label
    ).toBe('Starting')
  })
  test('stale and missing heartbeats override historical success', () => {
    for (const last_heartbeat of [
      null,
      'invalid',
      new Date(now - 91_000).toISOString(),
    ]) {
      expect(
        workerIngressStatus({ ...running, last_heartbeat }, now).label
      ).toBe('Unconfirmed')
    }
  })
  test('shows binding errors and missing certificates', () => {
    expect(
      workerIngressStatus(
        { ...running, public_ingress_last_error: 'Port 443 is occupied' },
        now
      ).description
    ).toBe('Port 443 is occupied')
    expect(
      workerIngressStatus(
        { ...running, public_ingress_certificate_count: 0 },
        now
      ).label
    ).toBe('Waiting for certificates')
  })
  test('a disabled worker does not retain a ready badge', () => {
    expect(
      workerIngressStatus({ ...running, public_ingress_enabled: false }, now)
        .label
    ).toBe('Disabled')
  })
  test('certificates alone do not imply an application route exists', () => {
    expect(
      workerIngressStatus({ ...running, public_ingress_route_count: 0 }, now)
        .label
    ).toBe('No public routes')
  })
  test('describes listeners without promising external reachability', () => {
    expect(workerIngressStatus(running, now).label).toBe('Listeners running')
  })
  test('does not imply all applications work when routes are excluded', () => {
    expect(
      workerIngressStatus(
        { ...running, public_ingress_unsupported_route_count: 2 },
        now
      ).label
    ).toBe('Some routes unavailable')
  })
})
