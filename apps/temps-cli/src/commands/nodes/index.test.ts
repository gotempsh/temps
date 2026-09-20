// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { capabilityRemedy, describeCapability } from './index.js'
import type { NodeCapabilityResponse } from '../../api/types.gen.js'

function makeCapability(overrides: Partial<NodeCapabilityResponse> = {}): NodeCapabilityResponse {
  return {
    local_workloads: true,
    active_worker_nodes: 0,
    schedulable: true,
    reason: null,
    setup_path: '/settings/nodes',
    can_manage_nodes: true,
    ...overrides,
  }
}

describe('capabilityRemedy', () => {
  test('tells an operator who can manage nodes how to add one', () => {
    const remedy = capabilityRemedy(
      makeCapability({ local_workloads: false, schedulable: false })
    )
    expect(remedy).toContain('temps join')
    expect(remedy).toContain('/settings/nodes')
  })

  test('does not send a credential without node permissions to a page that refuses it', () => {
    const remedy = capabilityRemedy(
      makeCapability({
        local_workloads: false,
        schedulable: false,
        can_manage_nodes: false,
      })
    )
    expect(remedy).toContain('Ask an administrator')
    expect(remedy).not.toContain('temps join')
  })
})

describe('describeCapability', () => {
  test('a single-binary install runs workloads on this host', () => {
    expect(describeCapability(makeCapability())).toBe('this host')
  })

  test('a control plane with workers names the workers only', () => {
    expect(
      describeCapability(
        makeCapability({ local_workloads: false, active_worker_nodes: 2 })
      )
    ).toBe('2 worker nodes')
  })

  test('one worker is not pluralized', () => {
    expect(
      describeCapability(
        makeCapability({ local_workloads: false, active_worker_nodes: 1 })
      )
    ).toBe('1 worker node')
  })

  test('both targets are listed when both exist', () => {
    expect(describeCapability(makeCapability({ active_worker_nodes: 3 }))).toBe(
      'this host + 3 worker nodes'
    )
  })

  test('an unschedulable install says so instead of listing nothing', () => {
    // A blank value here would read as "loading"; the operator needs to see
    // that nothing can run, not an empty cell.
    expect(
      describeCapability(
        makeCapability({ local_workloads: false, schedulable: false })
      )
    ).toBe('nowhere — no schedulable target')
  })
})
