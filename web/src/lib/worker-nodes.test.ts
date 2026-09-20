// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { NodeCapabilityResponse as NodeCapability } from '@/api/client/types.gen'
import {
  canAddWorkerNode,
  isWorkerNodeRequiredProblem,
  NODE_CAPABILITY_POLL_MS,
  nodeCapabilityRefetchInterval,
  problemErrorCode,
  sameOriginSetupPath,
  shouldPromptForFirstWorkerNode,
  shouldShowWorkerNodeBanner,
  WORKER_NODES_URL,
} from './worker-nodes'

function capability(overrides: Partial<NodeCapability> = {}): NodeCapability {
  return {
    local_workloads: true,
    active_worker_nodes: 0,
    schedulable: true,
    reason: null,
    setup_path: WORKER_NODES_URL,
    can_manage_nodes: true,
    ...overrides,
  }
}

describe('worker-node banner visibility', () => {
  test('stays hidden while the capability is unknown', () => {
    expect(shouldShowWorkerNodeBanner(undefined)).toBe(false)
    expect(shouldShowWorkerNodeBanner(null)).toBe(false)
  })

  test('stays hidden when the control plane runs workloads itself', () => {
    expect(shouldShowWorkerNodeBanner(capability())).toBe(false)
  })

  test('stays hidden when a worker node is carrying the work', () => {
    expect(
      shouldShowWorkerNodeBanner(
        capability({
          local_workloads: false,
          active_worker_nodes: 1,
          schedulable: true,
        })
      )
    ).toBe(false)
  })

  test('shows when nothing can run a workload', () => {
    expect(
      shouldShowWorkerNodeBanner(
        capability({
          local_workloads: false,
          active_worker_nodes: 0,
          schedulable: false,
          reason: 'No worker nodes have joined',
        })
      )
    ).toBe(true)
  })
})

describe('first-worker-node prompt on the Nodes page', () => {
  test('prompts only when no node joined and nothing runs locally', () => {
    const noLocal = capability({ local_workloads: false, schedulable: false })
    expect(shouldPromptForFirstWorkerNode(noLocal, 0)).toBe(true)
    expect(shouldPromptForFirstWorkerNode(noLocal, 1)).toBe(false)
    expect(shouldPromptForFirstWorkerNode(capability(), 0)).toBe(false)
    expect(shouldPromptForFirstWorkerNode(undefined, 0)).toBe(false)
  })
})

describe('who is offered the add-a-node action', () => {
  test('offers it to a user the server says can manage nodes', () => {
    expect(canAddWorkerNode(capability({ can_manage_nodes: true }))).toBe(true)
  })

  test('withholds it from a user who would hit a permission wall', () => {
    // The Worker Nodes page needs SettingsRead/SettingsWrite; linking a
    // project user there is a dead end, so they get "ask an administrator".
    expect(canAddWorkerNode(capability({ can_manage_nodes: false }))).toBe(
      false
    )
  })

  test('withholds it when the capability is unknown or predates the field', () => {
    expect(canAddWorkerNode(undefined)).toBe(false)
    expect(canAddWorkerNode(null)).toBe(false)
    expect(
      canAddWorkerNode({
        ...capability(),
        can_manage_nodes: undefined as unknown as boolean,
      })
    ).toBe(false)
  })
})

describe('capability polling', () => {
  test('polls while nothing can run, so a join clears the banner', () => {
    expect(
      nodeCapabilityRefetchInterval(
        capability({ local_workloads: false, schedulable: false })
      )
    ).toBe(NODE_CAPABILITY_POLL_MS)
  })

  test('stops polling once something can run the work', () => {
    expect(nodeCapabilityRefetchInterval(capability())).toBe(false)
  })

  test('does not poll before the first answer arrives', () => {
    expect(nodeCapabilityRefetchInterval(undefined)).toBe(false)
    expect(nodeCapabilityRefetchInterval(null)).toBe(false)
  })
})

describe('worker-node Problem detection', () => {
  test('reads a top-level error_code', () => {
    expect(
      isWorkerNodeRequiredProblem({ error_code: 'WORKER_NODE_REQUIRED' })
    ).toBe(true)
  })

  test('reads an extensions error_code', () => {
    expect(
      isWorkerNodeRequiredProblem({
        extensions: { error_code: 'WORKER_NODE_REQUIRED' },
      })
    ).toBe(true)
  })

  test('leaves other problems alone', () => {
    expect(
      isWorkerNodeRequiredProblem({ error_code: 'STEP_UP_REQUIRED' })
    ).toBe(false)
    expect(isWorkerNodeRequiredProblem({ detail: 'boom' })).toBe(false)
    expect(isWorkerNodeRequiredProblem(null)).toBe(false)
    expect(problemErrorCode('nope')).toBeUndefined()
  })
})

describe('sameOriginSetupPath', () => {
  test('keeps a plain same-origin path and falls back for anything else', () => {
    expect(sameOriginSetupPath('/settings/nodes')).toBe('/settings/nodes')
    for (const bad of [
      undefined,
      '',
      'settings',
      '//evil.example',
      'https://evil.example/x',
      '/x\\y',
      '/a b',
    ]) {
      expect(sameOriginSetupPath(bad)).toBe('/settings/nodes')
    }
  })
})
