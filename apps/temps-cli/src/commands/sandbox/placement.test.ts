// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { Command } from 'commander'
import {
  assertEvictConfirmable,
  nextAllowList,
  parsePageOption,
  registerSandboxCommands,
  resolveNodeIds,
  toSandboxView,
  CONTROL_PLANE_NODE_NAME,
} from './index.js'

const nodes = [
  {
    id: 0,
    name: 'control-plane',
    is_control_plane: true,
    status: 'active',
    allowed: true,
    eligible: true,
    reason: null,
    live_sandboxes: 1,
  },
  {
    id: 3,
    name: 'worker-1',
    is_control_plane: false,
    status: 'active',
    allowed: true,
    eligible: true,
    reason: null,
    live_sandboxes: 0,
  },
]

describe('resolveNodeIds', () => {
  it('accepts names, ids and control-plane aliases, deduplicated', () => {
    expect(resolveNodeIds(['worker-1', '0', 'local', '3'], nodes)).toEqual([3, 0])
  })

  it('reads a bare number as an id, like the server, even if a name matches', () => {
    const numeric = [
      ...nodes,
      {
        id: 5,
        name: '3',
        is_control_plane: false,
        status: 'active',
        allowed: true,
        eligible: true,
        reason: null,
        live_sandboxes: 0,
      },
    ]
    expect(resolveNodeIds(['3'], numeric)).toEqual([3])
    expect(resolveNodeIds(['5'], numeric)).toEqual([5])
  })

  it('rejects unknown nodes with the list of valid ones', () => {
    expect(() => resolveNodeIds(['worker-9'], nodes)).toThrow(
      "Unknown node 'worker-9'. Known nodes: control-plane (0), worker-1 (3)",
    )
  })
})

describe('toSandboxView node', () => {
  const base = {
    id: 'sbx_1',
    name: 'n',
    status: 'running',
    image: null,
    cwd: '/workspace',
    createdAt: 0,
    timeout: 1000,
  }

  it('shows the hosting node', () => {
    expect(toSandboxView({ ...base, node_id: 3, node_name: 'worker-1' }).node_name).toBe('worker-1')
  })

  it('treats servers without node fields as control-plane', () => {
    expect(toSandboxView(base).node_name).toBe(CONTROL_PLANE_NODE_NAME)
  })
})

describe('nextAllowList', () => {
  const all = [0, 3, 4]

  it('set replaces the list', () => {
    expect(nextAllowList('set', [0, 3], [4], all)).toEqual([4])
  })

  it('allow adds, and keeps "every node" as is', () => {
    expect(nextAllowList('allow', [0], [3, 0], all)).toEqual([0, 3])
    expect(nextAllowList('allow', null, [3], all)).toBeNull()
  })

  it('deny removes, starting from every node when unrestricted', () => {
    expect(nextAllowList('deny', [0, 3], [3], all)).toEqual([0])
    expect(nextAllowList('deny', null, [0], all)).toEqual([3, 4])
  })
})

describe('sandbox nodes command wiring', () => {
  const program = new Command()
  registerSandboxCommands(program)
  const nodesCmd = program.commands
    .find((c) => c.name() === 'sandbox')
    ?.commands.find((c) => c.name() === 'nodes')

  it('keeps --json off the parent so subcommands receive it', () => {
    expect(nodesCmd).toBeDefined()
    expect(nodesCmd!.options.map((o) => o.long)).not.toContain('--json')
  })

  it('gives every subcommand its own --json', () => {
    for (const sub of nodesCmd!.commands) {
      expect(sub.options.map((o) => o.long)).toContain('--json')
    }
  })

  it('parses --json after a subcommand argument', async () => {
    const show = nodesCmd!.commands.find((c) => c.name() === 'show')!
    let seen: Record<string, unknown> | undefined
    show.action((_node: string, opts: Record<string, unknown>) => {
      seen = opts
    })
    await program.parseAsync(['sandbox', 'nodes', 'show', 'worker-1', '--json'], { from: 'user' })
    expect(seen?.json).toBe(true)
  })
})

describe('evict confirmation', () => {
  it('prompts only on an interactive terminal without --json', () => {
    expect(() => assertEvictConfirmable({}, true)).not.toThrow()
  })

  it('refuses without --force when it cannot ask', () => {
    expect(() => assertEvictConfirmable({}, false)).toThrow('--force')
    expect(() => assertEvictConfirmable({ json: true }, true)).toThrow('--force')
  })

  it('--force skips the question everywhere', () => {
    expect(() => assertEvictConfirmable({ force: true, json: true }, false)).not.toThrow()
  })
})

describe('parsePageOption', () => {
  it('accepts positive integers and leaves unset options unset', () => {
    expect(parsePageOption('3', '--page')).toBe(3)
    expect(parsePageOption(undefined, '--page')).toBeUndefined()
  })

  it('rejects zero, negatives, fractions and text with the flag name', () => {
    for (const raw of ['0', '-1', '1.5', 'two']) {
      expect(() => parsePageOption(raw, '--page-size')).toThrow('--page-size')
    }
  })
})
