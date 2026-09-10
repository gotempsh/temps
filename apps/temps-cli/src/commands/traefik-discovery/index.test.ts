// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { routeStateBadgeInput, formatTarget, describeConflict } from './index.js'
import type {
  TraefikDiscoveredRouteResponse,
  TraefikDiscoveryConflictResponse,
} from '../../api/types.gen.js'

function makeRoute(
  overrides: Partial<TraefikDiscoveredRouteResponse> = {}
): TraefikDiscoveredRouteResponse {
  const now = new Date().toISOString()
  return {
    id: 1,
    host: 'app.example.com',
    router_name: 'app',
    target_container_id: 'abc123',
    target_container_name: 'whoami',
    target_port: 80,
    target_host_port: null,
    network: 'temps',
    tls: false,
    enabled: true,
    active: true,
    inactive_reason: null,
    contested_by: [],
    last_seen_at: now,
    created_at: now,
    updated_at: now,
    ...overrides,
  }
}

function makeConflict(
  overrides: Partial<TraefikDiscoveryConflictResponse> = {}
): TraefikDiscoveryConflictResponse {
  return {
    host: 'app.example.com',
    container_id: 'loser-id',
    container_name: 'whoami-2',
    router_name: 'app',
    reason: 'claimed_by_another_container',
    detail: "host already claimed by discovered container 'whoami'",
    winner_container_name: 'whoami',
    ...overrides,
  }
}

describe('routeStateBadgeInput', () => {
  test('an enabled, uncontested route is active', () => {
    expect(routeStateBadgeInput(makeRoute())).toBe('active')
  })

  test('a suppressed route is inactive', () => {
    expect(routeStateBadgeInput(makeRoute({ enabled: false, active: false }))).toBe('inactive')
  })

  test('an enabled but contested route is not shown as plainly active', () => {
    // A contested host routes to exactly one container; the operator needs to
    // see that someone else's container is silently losing.
    expect(routeStateBadgeInput(makeRoute({ contested_by: ['whoami-2'] }))).toBe('pending')
  })

  test('suppression wins over contention', () => {
    const route = makeRoute({ enabled: false, active: false, contested_by: ['whoami-2'] })
    expect(routeStateBadgeInput(route)).toBe('inactive')
  })
})

describe('formatTarget', () => {
  test('shows container and container-internal port', () => {
    expect(formatTarget(makeRoute())).toBe('whoami:80')
  })

  test('adds the published host port when the container publishes one', () => {
    // Baremetal installs route through the host port because the proxy cannot
    // resolve container names; hiding it makes those setups undebuggable.
    expect(formatTarget(makeRoute({ target_host_port: 18080 }))).toBe(
      'whoami:80 (host :18080)'
    )
  })

  test('port 0 is never special-cased away', () => {
    expect(formatTarget(makeRoute({ target_port: 8443, target_host_port: 0 }))).toBe(
      'whoami:8443 (host :0)'
    )
  })
})

describe('describeConflict', () => {
  test('container-vs-container collision names the winner', () => {
    expect(describeConflict(makeConflict())).toBe(
      "app.example.com — 'whoami-2' lost to 'whoami'"
    )
  })

  test('a host owned by a Temps route explains the precedence rule', () => {
    const conflict = makeConflict({
      host: 'console.example.com',
      container_name: 'impostor',
      reason: 'owned_by_temps_route',
      detail: 'host already belongs to a Temps-managed route',
      winner_container_name: null,
    })
    expect(describeConflict(conflict)).toBe(
      "console.example.com — 'impostor' cannot take a Temps-managed host"
    )
  })

  test('an unrecognized reason still explains itself rather than rendering blank', () => {
    // Forward compatibility: a newer server may add a conflict kind. The line
    // must stay useful instead of collapsing to an empty string.
    const conflict = makeConflict({
      reason: 'some_future_reason',
      detail: 'something new happened',
      winner_container_name: null,
    })
    expect(describeConflict(conflict)).toBe(
      "app.example.com — 'whoami-2': something new happened"
    )
  })

  test('a claimed_by_another_container conflict without a winner falls back to the detail', () => {
    const conflict = makeConflict({ winner_container_name: null })
    expect(describeConflict(conflict)).toContain('host already claimed by discovered container')
  })
})
