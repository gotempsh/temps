// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { NodeCapability } from '@/api/nodeCapability'
import {
  isWorkerNodeRequiredProblem,
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
