// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, spyOn, test } from 'bun:test'
import { Command } from 'commander'
import { registerNodesCommands } from './index.js'
import {
  buildNodeResourcesRequest,
  needsCurrentReservation,
  parseResourceFlags,
  parseResourceNodeId,
} from './resources.js'
import type { NodeResourcesResponse } from '../../api/types.gen.js'

const current: NodeResourcesResponse = {
  node_id: 3,
  resource_policy: 'best_effort',
  system_reserved: { cpu_millis: 500, memory_mb: 1024 },
  system_reserved_is_default: true,
}

function nodesCommand(): Command {
  const program = new Command()
  registerNodesCommands(program)
  return program.commands.find((c) => c.name() === 'nodes')!
}

describe('registration', () => {
  test('resources show/set and state-status are registered with their flags', () => {
    const nodes = nodesCommand()
    const resources = nodes.commands.find((c) => c.name() === 'resources')!
    expect(resources.commands.map((c) => c.name())).toEqual(expect.arrayContaining(['show', 'set']))
    const set = resources.commands.find((c) => c.name() === 'set')!
    expect(set.options.map((o) => o.long)).toEqual(
      expect.arrayContaining(['--policy', '--reserve-cpu', '--reserve-memory', '--json'])
    )
    expect(nodes.commands.some((c) => c.name() === 'state-status')).toBe(true)
  })
})

describe('parseResourceNodeId', () => {
  test('0 and control-plane both mean the control plane', () => {
    expect(parseResourceNodeId('0')).toBe(0)
    expect(parseResourceNodeId('control-plane')).toBe(0)
    expect(parseResourceNodeId('7')).toBe(7)
  })

  test('refuses anything that is not a whole id', () => {
    for (const bad of ['-1', '1.5', 'worker-1', '12abc', '']) {
      expect(parseResourceNodeId(bad)).toBeNull()
    }
  })
})

describe('parseResourceFlags', () => {
  test('requires at least one change', () => {
    expect('error' in parseResourceFlags({})).toBe(true)
  })

  test('accepts both policies and normalises the hyphen', () => {
    expect(parseResourceFlags({ policy: 'enforced' })).toEqual({ update: { policy: 'enforced' } })
    expect(parseResourceFlags({ policy: 'best-effort' })).toEqual({ update: { policy: 'best_effort' } })
    expect('error' in parseResourceFlags({ policy: 'strict' })).toBe(true)
  })

  test('reservations are non-negative whole numbers', () => {
    expect(parseResourceFlags({ reserveCpu: '0', reserveMemory: '2048' })).toEqual({
      update: { reserveCpuMillis: 0, reserveMemoryMb: 2048 },
    })
    expect('error' in parseResourceFlags({ reserveCpu: '0.5' })).toBe(true)
    expect('error' in parseResourceFlags({ reserveMemory: '2GB' })).toBe(true)
  })
})

describe('buildNodeResourcesRequest', () => {
  test('a policy change alone leaves the reservation out of the body', () => {
    expect(buildNodeResourcesRequest({ policy: 'enforced' }, undefined)).toEqual({ resource_policy: 'enforced' })
  })

  test('one reservation half keeps the node\'s current other half', () => {
    expect(needsCurrentReservation({ reserveCpuMillis: 750 })).toBe(true)
    expect(buildNodeResourcesRequest({ reserveCpuMillis: 750 }, current)).toEqual({
      system_reserved: { cpu_millis: 750, memory_mb: 1024 },
    })
    expect(buildNodeResourcesRequest({ reserveMemoryMb: 512 }, current)).toEqual({
      system_reserved: { cpu_millis: 500, memory_mb: 512 },
    })
  })

  test('both halves need no lookup', () => {
    expect(needsCurrentReservation({ reserveCpuMillis: 1, reserveMemoryMb: 2 })).toBe(false)
    expect(needsCurrentReservation({ policy: 'enforced' })).toBe(false)
    expect(buildNodeResourcesRequest({ policy: 'best_effort', reserveCpuMillis: 1, reserveMemoryMb: 2 }, undefined)).toEqual({
      resource_policy: 'best_effort',
      system_reserved: { cpu_millis: 1, memory_mb: 2 },
    })
  })
})

describe('invalid flags fail before any request', () => {
  test.each([
    [['nodes', 'resources', 'show', 'worker-a'], 'Invalid node'],
    [['nodes', 'resources', 'set', '2'], 'Pass at least one'],
    [['nodes', 'resources', 'set', '2', '--policy', 'strict'], 'Unknown --policy'],
    [['nodes', 'resources', 'set', '2', '--reserve-cpu', 'lots'], '--reserve-cpu'],
  ] as const)('%p', async (argv, message) => {
    const saved = { url: process.env.TEMPS_API_URL, token: process.env.TEMPS_TOKEN, exitCode: process.exitCode }
    process.env.TEMPS_API_URL = 'http://127.0.0.1:9'
    process.env.TEMPS_TOKEN = 'test-token-never-sent'
    process.exitCode = 0
    const stderr: string[] = []
    const errSpy = spyOn(console, 'error').mockImplementation((...args: unknown[]) => {
      stderr.push(args.join(' '))
    })
    const fetchSpy = spyOn(globalThis, 'fetch')
    const exitSpy = spyOn(process, 'exit').mockImplementation(((code?: number) => {
      throw new Error(`process.exit(${code}) called: validation ran after auth`)
    }) as typeof process.exit)
    try {
      const program = new Command().exitOverride()
      registerNodesCommands(program)
      await program.parseAsync([...argv], { from: 'user' })
      expect(process.exitCode).toBe(1)
      expect(stderr.join('\n')).toContain(message)
      expect(fetchSpy.mock.calls.length).toBe(0)
    } finally {
      errSpy.mockRestore()
      fetchSpy.mockRestore()
      exitSpy.mockRestore()
      process.exitCode = saved.exitCode ?? 0
      if (saved.url === undefined) delete process.env.TEMPS_API_URL
      else process.env.TEMPS_API_URL = saved.url
      if (saved.token === undefined) delete process.env.TEMPS_TOKEN
      else process.env.TEMPS_TOKEN = saved.token
    }
  })
})
