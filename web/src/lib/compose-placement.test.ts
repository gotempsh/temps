// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import {
  composeHomeSummary,
  composeMoveCandidates,
  composeMoveLosesData,
  composeRunningNodeId,
} from './compose-placement'

describe('composeHomeSummary', () => {
  test('an unpinned stack says where its first deployment goes', () => {
    const summary = composeHomeSummary({ pinned: false })
    expect(summary.title).toBe('Not deployed yet')
    expect(summary.detail).toContain('Target nodes')
    // Empty constraints do not promise the control plane: the scheduler
    // may pick a worker.
    expect(summary.detail).toContain('control plane or any worker')
  })

  test('a pending move away from an unavailable node does not claim it runs there', () => {
    const summary = composeHomeSummary({
      pinned: true,
      home_node_name: 'worker-b',
      moving_from_node_id: 3,
      moving_from_node_name: 'worker-a',
      moving_from_node_status: 'offline',
    })
    expect(summary.title).toBe('Moving from worker-a (offline) to worker-b')
    expect(summary.detail).toContain('running state is unknown')
    expect(summary.detail).toContain('volumes are on worker-a')
    expect(summary.detail).not.toContain('keeps running')
  })

  test('a pending move names its active source and its destination', () => {
    const summary = composeHomeSummary({
      pinned: true,
      home_node_name: 'worker-b',
      moving_from_node_id: 3,
      moving_from_node_name: 'worker-a',
      moving_from_node_status: 'active',
    })
    expect(summary.title).toBe('Moving from worker-a to worker-b')
    expect(summary.detail).toContain('next deployment starts it on worker-b')
    expect(summary.detail).toContain('data on worker-a stays there')
  })

  test('a pinned stack names its home and why it stays', () => {
    const summary = composeHomeSummary({
      pinned: true,
      home_node_name: 'worker-a',
    })
    expect(summary.title).toBe('Runs on worker-a')
    expect(summary.detail).toContain('volumes live there')
    expect(summary.detail).toContain('Move stack')
  })
})

describe('moving a Compose stack', () => {
  const nodes = [{ id: 0 }, { id: 3 }, { id: 4 }]

  test('an unpinned stack has no data, so no move loses any', () => {
    const placement = { pinned: false }
    expect(composeRunningNodeId(placement)).toBeNull()
    expect(composeMoveLosesData(placement, 3)).toBe(false)
    expect(composeMoveCandidates(placement, nodes)).toEqual(nodes)
  })

  test('moving a stack off its home loses its data there', () => {
    const placement = { pinned: true, home_node_id: 0 }
    expect(composeRunningNodeId(placement)).toBe(0)
    expect(composeMoveLosesData(placement, 3)).toBe(true)
    expect(composeMoveCandidates(placement, nodes).map((n) => n.id)).toEqual([
      3, 4,
    ])
  })

  test('moving back to the host a pending move is leaving cancels it without losing data', () => {
    const placement = { pinned: true, home_node_id: 3, moving_from_node_id: 0 }
    expect(composeRunningNodeId(placement)).toBe(0)
    expect(composeMoveLosesData(placement, 0)).toBe(false)
    expect(composeMoveLosesData(placement, 4)).toBe(true)
    expect(composeMoveCandidates(placement, nodes).map((n) => n.id)).toEqual([
      0, 4,
    ])
  })

  test('a build-only node is never offered as a target', () => {
    const placement = { pinned: true, home_node_id: 0 }
    const withBuilder = [
      { id: 0, labels: {} },
      { id: 3, labels: { 'temps.sh/role': 'builder' } },
      { id: 4, labels: { 'temps.sh/role': 'dedicated' } },
      { id: 5, labels: null },
    ]
    expect(
      composeMoveCandidates(placement, withBuilder).map((n) => n.id)
    ).toEqual([4, 5])
  })
})
