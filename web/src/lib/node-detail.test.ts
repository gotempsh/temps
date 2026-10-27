// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  nodeOverviewAlerts,
  nodeRouteOf,
  nodeSectionUrl,
  nodeSections,
  resolveNodeSection,
  type NodeAlertInput,
} from './node-detail'

const NOW = Date.parse('2026-10-27T12:00:00Z')

function node(
  overrides: Partial<NodeAlertInput['node']> = {}
): NodeAlertInput['node'] {
  return {
    id: 3,
    name: 'worker-a',
    status: 'active',
    last_heartbeat: '2026-10-27T11:59:50Z',
    compute_cidr: '10.202.0.0/24',
    overlay_status: 'ready',
    overlay_last_error: null,
    public_ingress_enabled: false,
    public_ingress_last_error: null,
    ...overrides,
  }
}

describe('nodeSections', () => {
  test('only workers have builds; edge nodes have no networking', () => {
    const titles = (role: string) =>
      nodeSections(role).map((link) => link.section)
    expect(titles('worker')).toEqual([
      'overview',
      'monitoring',
      'networking',
      'builds',
      'scheduling',
    ])
    expect(titles('control-plane')).not.toContain('builds')
    expect(titles('edge')).toEqual(['overview', 'monitoring', 'scheduling'])
  })

  test('a section the node does not have opens the overview', () => {
    expect(resolveNodeSection('builds', 'worker')).toBe('builds')
    expect(resolveNodeSection('builds', 'edge')).toBe('overview')
    expect(resolveNodeSection('nonsense', 'worker')).toBe('overview')
    expect(resolveNodeSection(undefined, 'worker')).toBe('overview')
  })
})

describe('node URLs', () => {
  test('the overview is the node URL itself', () => {
    expect(nodeSectionUrl(3, 'overview')).toBe('/settings/nodes/3')
    expect(nodeSectionUrl(3, 'monitoring')).toBe('/settings/nodes/3/monitoring')
  })

  test('only node pages are recognised', () => {
    expect(nodeRouteOf('/settings/nodes/3')).toEqual({
      nodeId: 3,
      segment: undefined,
    })
    expect(nodeRouteOf('/settings/nodes/3/networking')).toEqual({
      nodeId: 3,
      segment: 'networking',
    })
    expect(nodeRouteOf('/settings/nodes')).toBeNull()
    expect(nodeRouteOf('/settings/nodes/ingress')).toBeNull()
  })
})

describe('nodeOverviewAlerts', () => {
  test('a healthy node shows nothing', () => {
    expect(
      nodeOverviewAlerts({
        node: node(),
        dns: { enabled: true, running: true, applicationsWithoutResolver: 0 },
        firingAlarms: [],
        now: NOW,
      })
    ).toEqual([])
  })

  test('problems come most severe first, each pointing at its fix', () => {
    const alerts = nodeOverviewAlerts({
      node: node({
        overlay_status: 'unavailable',
        overlay_last_error: 'subnet overlaps a Docker network',
      }),
      dns: {
        enabled: true,
        running: false,
        lastError: 'bind: address in use',
        applicationsWithoutResolver: 2,
      },
      firingAlarms: [
        { id: 9, title: 'Disk above 90%', severity: 'warning', message: null },
      ],
      now: NOW,
    })

    expect(alerts.map((alert) => alert.key)).toEqual([
      'dns-down',
      'alarm-9',
      'overlay',
      'dns-stale',
    ])
    expect(alerts[0].description).toContain('bind: address in use')
    expect(alerts[0].action?.to).toBe('/settings/nodes/3/networking')
    expect(alerts[1].action?.to).toBe('/settings/nodes/3/monitoring')
    expect(alerts[3].title).toBe(
      '2 applications here cannot resolve service names'
    )
  })

  test('a silent agent is reported, but not while the node is draining', () => {
    const stale = node({ last_heartbeat: '2026-10-27T11:00:00Z' })
    expect(
      nodeOverviewAlerts({ node: stale, now: NOW }).map((a) => a.key)
    ).toEqual(['unreachable'])
    expect(
      nodeOverviewAlerts({ node: stale, now: NOW })[0].description
    ).toStartWith('No heartbeat for 1 hour.')
    expect(
      nodeOverviewAlerts({ node: { ...stale, status: 'drained' }, now: NOW })
    ).toEqual([])
  })

  test('cluster DNS being off is not a problem of this node', () => {
    expect(
      nodeOverviewAlerts({
        node: node(),
        dns: { enabled: false, running: false, applicationsWithoutResolver: 3 },
        now: NOW,
      })
    ).toEqual([])
  })
})
