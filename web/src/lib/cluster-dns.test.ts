// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { placementNeedsClusterDns } from './cluster-dns'

describe('placementNeedsClusterDns', () => {
  test('a standalone service on a worker is reached by name from other nodes', () => {
    expect(
      placementNeedsClusterDns({ topology: 'standalone', nodeId: 2 })
    ).toBe(true)
  })

  test('a standalone service on the control plane needs nothing', () => {
    expect(
      placementNeedsClusterDns({ topology: 'standalone', nodeId: null })
    ).toBe(false)
  })

  test('a cluster spanning machines stores temps.local member names', () => {
    expect(
      placementNeedsClusterDns({
        topology: 'cluster',
        memberNodeIds: [null, 1, 2],
      })
    ).toBe(true)
    expect(
      placementNeedsClusterDns({
        topology: 'cluster',
        memberNodeIds: [1, 2, 3],
      })
    ).toBe(true)
  })

  test('a cluster on one machine keeps container names', () => {
    expect(
      placementNeedsClusterDns({
        topology: 'cluster',
        memberNodeIds: [undefined, null, null],
      })
    ).toBe(false)
    expect(
      placementNeedsClusterDns({ topology: 'cluster', memberNodeIds: [] })
    ).toBe(false)
  })
})
