// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { telemetryFreshnessSummary } from './telemetry-freshness'

describe('telemetryFreshnessSummary', () => {
  test('distinguishes fresh container telemetry from missing RustFS telemetry', () => {
    expect(
      telemetryFreshnessSummary(
        'rustfs',
        ['container.cpu_percent', 'container.memory_bytes'],
        '4s ago'
      )
    ).toBe(
      'Container telemetry active · RustFS application metrics not received'
    )
  })

  test('recognizes a realistic metric from the pinned RustFS image', () => {
    expect(
      telemetryFreshnessSummary(
        'rustfs',
        ['container.cpu_percent', 'rustfs_cluster_capacity_used_bytes'],
        '8s ago'
      )
    ).toBe('RustFS application metrics present · telemetry received 8s ago')
  })

  test('keeps non-RustFS freshness wording unchanged', () => {
    expect(
      telemetryFreshnessSummary('postgres', ['pg.connections'], '12s ago')
    ).toBe('last received 12s ago')
  })
})
