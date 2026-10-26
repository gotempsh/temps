// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { NodeCapabilityResponse as NodeCapability } from '@/api/client/types.gen'
import {
  canAddWorkerNode,
  CLUSTER_DNS_URL,
  nodesAdvancedSection,
  isWorkerNodeRequiredProblem,
  NODE_CAPABILITY_POLL_MS,
  NODE_HEARTBEAT_STALE_SECS,
  nodeIsUnreachable,
  nodeRemoval,
  nodeRemovalBlocker,
  type NodeRemovalFacts,
  nodeCapabilityRefetchInterval,
  problemErrorCode,
  sameOriginSetupPath,
  servingOnUnreachableNode,
  shouldPromptForFirstWorkerNode,
  shouldShowWorkerNodeBanner,
  unremovedNodeContainers,
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

describe('node removal refused for unconfirmed containers', () => {
  test('lists the containers from the Problem body', () => {
    const containers = ["container 'app-1' (abc) of deployment 4 in project 2"]
    expect(
      unremovedNodeContainers({
        title: 'Node Still Holds Containers',
        unremoved_count: 1,
        unremoved_containers: containers,
      })
    ).toEqual({ count: 1, lines: containers })
    expect(
      unremovedNodeContainers({
        extensions: { unremoved_containers: containers },
      })
    ).toEqual({ count: 1, lines: containers })
  })

  test('counts every container, not the bounded description list', () => {
    const lines = [
      "container 'app-1' (abc) of deployment 4 in project 2",
      'and 119 more container(s) not listed here',
    ]
    expect(
      unremovedNodeContainers({
        unremoved_count: 120,
        unremoved_containers: lines,
      })
    ).toEqual({ count: 120, lines })
  })

  test('is undefined for any other refusal', () => {
    expect(
      unremovedNodeContainers({ title: 'Node Has Active Containers' })
    ).toBeUndefined()
    expect(
      unremovedNodeContainers({ unremoved_containers: [] })
    ).toBeUndefined()
    expect(unremovedNodeContainers(null)).toBeUndefined()
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

describe('nodeIsUnreachable', () => {
  const now = Date.parse('2026-10-20T12:00:00Z')
  const ago = (seconds: number) => new Date(now - seconds * 1000).toISOString()

  test('a never-heard-from node is unreachable; a fresh heartbeat is not', () => {
    expect(nodeIsUnreachable({ last_heartbeat: null }, now)).toBe(true)
    expect(nodeIsUnreachable({ last_heartbeat: ago(1) }, now)).toBe(false)
  })

  test('a node silent past the threshold is unreachable', () => {
    expect(
      nodeIsUnreachable(
        { last_heartbeat: ago(NODE_HEARTBEAT_STALE_SECS + 1) },
        now
      )
    ).toBe(true)
    expect(
      nodeIsUnreachable(
        { last_heartbeat: ago(NODE_HEARTBEAT_STALE_SECS - 1) },
        now
      )
    ).toBe(false)
  })
})

describe('servingOnUnreachableNode', () => {
  test('reads the environments an unreachable node still serves', () => {
    expect(
      servingOnUnreachableNode({
        force_removes_serving: true,
        serving_containers: 2,
        serving_environments: ["environment 'prod' of project 'api'", 7],
      })
    ).toEqual({
      count: 2,
      environments: ["environment 'prod' of project 'api'"],
    })
    expect(
      servingOnUnreachableNode({
        extensions: {
          force_removes_serving: true,
          serving_environments: ['a', 'b'],
        },
      })
    ).toEqual({ count: 2, environments: ['a', 'b'] })
  })

  test('ignores a still-serving refusal for a reachable node', () => {
    expect(
      servingOnUnreachableNode({
        serving_containers: 1,
        title: 'Node Has Active Containers',
      })
    ).toBeUndefined()
    expect(servingOnUnreachableNode(null)).toBeUndefined()
  })
})

describe('nodeRemoval', () => {
  const now = Date.parse('2026-10-20T12:00:00Z')
  const fresh = new Date(now - 5_000).toISOString()
  const stale = new Date(
    now - (NODE_HEARTBEAT_STALE_SECS + 60) * 1000
  ).toISOString()
  const worker = (
    overrides: Partial<NodeRemovalFacts> = {}
  ): NodeRemovalFacts => ({
    id: 7,
    role: 'worker',
    status: 'active',
    last_heartbeat: fresh,
    serving_containers: 0,
    live_sandboxes: 0,
    ...overrides,
  })

  test('the control plane entry offers no removal', () => {
    expect(
      nodeRemoval(
        worker({
          id: 0,
          role: 'control-plane',
          serving_containers: null,
          live_sandboxes: null,
        }),
        now
      )
    ).toEqual({ kind: 'none' })
    expect(nodeRemoval(worker({ role: 'control-plane' }), now).kind).toBe(
      'none'
    )
  })

  test('an empty, reachable active node is removable directly', () => {
    expect(nodeRemoval(worker(), now)).toEqual({ kind: 'remove' })
    expect(nodeRemoval(worker({ status: 'draining' }), now)).toEqual({
      kind: 'remove',
    })
    expect(nodeRemoval(worker({ status: 'drained' }), now)).toEqual({
      kind: 'remove',
    })
  })

  test('a reachable node with serving containers has to be drained first', () => {
    const removal = nodeRemoval(worker({ serving_containers: 2 }), now)
    expect(removal).toEqual({
      kind: 'drain-first',
      containers: 2,
      draining: false,
    })
    expect(nodeRemovalBlocker(removal)).toBe(
      'Drain first — it has 2 containers'
    )
    expect(
      nodeRemovalBlocker(nodeRemoval(worker({ serving_containers: 1 }), now))
    ).toBe('Drain first — it has 1 container')
  })

  test('a draining node with containers left says the drain is under way', () => {
    const removal = nodeRemoval(
      worker({ status: 'draining', serving_containers: 3 }),
      now
    )
    expect(removal).toEqual({
      kind: 'drain-first',
      containers: 3,
      draining: true,
    })
    expect(nodeRemovalBlocker(removal)).toBe('Draining — 3 containers left')
  })

  test('sandboxes block removal, reachable or not', () => {
    const removal = nodeRemoval(worker({ live_sandboxes: 2 }), now)
    expect(removal).toEqual({ kind: 'sandboxes', sandboxes: 2 })
    expect(nodeRemovalBlocker(removal)).toBe(
      'Hosts 2 sandboxes — destroy them first (draining does not move sandboxes)'
    )
    expect(
      nodeRemovalBlocker(nodeRemoval(worker({ live_sandboxes: 1 }), now))
    ).toBe(
      'Hosts 1 sandbox — destroy it first (draining does not move sandboxes)'
    )
    expect(
      nodeRemoval(
        worker({
          last_heartbeat: stale,
          serving_containers: 4,
          live_sandboxes: 1,
        }),
        now
      )
    ).toEqual({ kind: 'sandboxes', sandboxes: 1 })
  })

  test('an unreachable or offline node is removable whatever it serves', () => {
    // The server asks before it forcibly removes containers still serving.
    expect(
      nodeRemoval(worker({ last_heartbeat: stale, serving_containers: 4 }), now)
    ).toEqual({ kind: 'remove' })
    expect(
      nodeRemoval(worker({ last_heartbeat: null, status: 'draining' }), now)
    ).toEqual({ kind: 'remove' })
    expect(
      nodeRemoval(worker({ status: 'offline', serving_containers: 1 }), now)
    ).toEqual({ kind: 'remove' })
  })

  test('a reachable node in another state offers no removal', () => {
    expect(nodeRemoval(worker({ status: 'pending' }), now)).toEqual({
      kind: 'none',
    })
  })

  test('an older server without the counts falls back to the caller', () => {
    for (const facts of [
      worker({ serving_containers: undefined, live_sandboxes: undefined }),
      worker({ serving_containers: null }),
      worker({ live_sandboxes: null }),
      worker({ last_heartbeat: stale, serving_containers: undefined }),
    ]) {
      expect(nodeRemoval(facts, now)).toEqual({ kind: 'unknown' })
    }
  })

  test('only blocked removals have a blocker line', () => {
    expect(nodeRemovalBlocker({ kind: 'remove' })).toBeNull()
    expect(nodeRemovalBlocker({ kind: 'none' })).toBeNull()
    expect(nodeRemovalBlocker({ kind: 'unknown' })).toBeNull()
  })
})

describe('nodesAdvancedSection', () => {
  test('names the advanced card a hash links to', () => {
    expect(nodesAdvancedSection('#cluster-dns')).toBe('cluster-dns')
    expect(nodesAdvancedSection('cluster-trust')).toBe('cluster-trust')
    expect(CLUSTER_DNS_URL).toBe('/settings/nodes#cluster-dns')
  })

  test('ignores hashes that are not an advanced card', () => {
    expect(nodesAdvancedSection('')).toBeNull()
    expect(nodesAdvancedSection('#ci-builder')).toBeNull()
    expect(nodesAdvancedSection('#cluster-dns-x')).toBeNull()
  })
})
