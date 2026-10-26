// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  clusterNetworkFormSchema,
  nodeOverlayStatus,
  validatePoolInput,
  type ClusterNetworkNode,
} from './cluster-network'

const node = (overrides: Partial<ClusterNetworkNode>): ClusterNetworkNode => ({
  id: 1,
  name: 'worker-1',
  overlay_network_name: 'temps',
  compute_cidr: '10.202.0.0/24',
  overlay_status: null,
  overlay_last_error: null,
  overlay_remediation: null,
  in_use: false,
  in_use_reason: null,
  occupied_ranges: [],
  ...overrides,
})

describe('validatePoolInput', () => {
  test('accepts a private pool with a longer per-node prefix', () => {
    expect(validatePoolInput('10.202.0.0/16', '24')).toBeNull()
    expect(validatePoolInput('172.16.0.0/12', '24')).toBeNull()
    expect(validatePoolInput(' 192.168.0.0/16 ', '24')).toBeNull()
  })

  test('names what is wrong', () => {
    expect(validatePoolInput('10.202.0.0', '24')).toContain('IPv4 network')
    expect(validatePoolInput('10.300.0.0/16', '24')).toContain('IPv4 network')
    expect(validatePoolInput('8.8.0.0/16', '24')).toContain('RFC 1918')
    expect(validatePoolInput('172.32.0.0/16', '24')).toContain('RFC 1918')
    expect(validatePoolInput('10.202.1.0/16', '24')).toContain('Host bits')
    expect(validatePoolInput('10.202.0.0/16', '16')).toContain('longer')
    expect(validatePoolInput('10.202.0.0/16', '31')).toContain('up to 30')
    expect(validatePoolInput('10.202.0.0/16', 'x')).toContain('up to 30')
  })
})

describe('clusterNetworkFormSchema', () => {
  test('puts each problem on the field it is about', () => {
    expect(
      clusterNetworkFormSchema.safeParse({
        poolCidr: '10.202.0.0/16',
        nodePrefix: '24',
      }).success
    ).toBe(true)
    const pool = clusterNetworkFormSchema.safeParse({
      poolCidr: '8.8.0.0/16',
      nodePrefix: '24',
    })
    expect(pool.success).toBe(false)
    expect(pool.error?.issues[0].path).toEqual(['poolCidr'])
    expect(pool.error?.issues[0].message).toContain('RFC 1918')
    const prefix = clusterNetworkFormSchema.safeParse({
      poolCidr: '10.202.0.0/16',
      nodePrefix: '16',
    })
    expect(prefix.error?.issues[0].path).toEqual(['nodePrefix'])
  })
})

describe('nodeOverlayStatus', () => {
  test('an unavailable overlay shows the reported cause and fix', () => {
    const cause =
      "overlay subnet 10.202.0.0/24 overlaps Docker network 'shop_default'. Fix: change the pool"
    const view = nodeOverlayStatus(
      node({ overlay_status: 'unavailable', overlay_last_error: cause })
    )
    expect(view.tone).toBe('warn')
    expect(view.label).toBe('Cross-node network unavailable')
    expect(view.description).toBe(cause)
  })

  test('ready, never reported and unallocated nodes', () => {
    expect(nodeOverlayStatus(node({ overlay_status: 'ready' })).tone).toBe('ok')
    expect(nodeOverlayStatus(node({})).label).toBe('Not reported')
    expect(nodeOverlayStatus(node({ compute_cidr: null })).label).toBe(
      'No subnet'
    )
  })

  test('an automatic move is mentioned', () => {
    const view = nodeOverlayStatus(
      node({
        overlay_status: 'ready',
        compute_cidr: '10.202.16.0/24',
        overlay_remediation:
          "moved from 10.202.0.0/24 to 10.202.16.0/24: 10.202.0.0/24 overlaps Docker network 'shop_default' (10.202.0.0/20)",
      })
    )
    expect(view.description).toContain('Automatically moved from 10.202.0.0/24')
  })
})
