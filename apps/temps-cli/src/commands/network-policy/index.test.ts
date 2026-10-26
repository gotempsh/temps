// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import {
  buildCreateRuleBody,
  buildProfileBody,
  formatPorts,
  parseId,
  registerNetworkPolicyCommands,
  requireOneOf,
  SUBJECT_KINDS,
} from './index.js'

const baseRule = {
  subjectKind: 'sandbox',
  subjectId: '7',
  direction: 'egress',
  peerKind: 'cidr',
  peer: '10.0.0.0/8',
  action: 'deny',
}

describe('requireOneOf', () => {
  test('accepts an allowed value', () => {
    expect(requireOneOf('--subject-kind', 'function', SUBJECT_KINDS)).toBe('function')
  })

  test('names the flag and every accepted value when rejecting', () => {
    expect(() => requireOneOf('--subject-kind', 'vm', SUBJECT_KINDS)).toThrow(
      "Invalid --subject-kind 'vm': expected one of deployment, sandbox, service, function, job",
    )
  })

  test('is case-sensitive like the server', () => {
    expect(() => requireOneOf('--subject-kind', 'Sandbox', SUBJECT_KINDS)).toThrow()
  })
})

describe('parseId', () => {
  test('parses a positive integer', () => {
    expect(parseId('rule id', '42')).toBe(42)
  })

  test.each(['0', '-1', '1.5', 'abc', ''])('rejects %p', (value) => {
    expect(() => parseId('rule id', value)).toThrow('Invalid rule id')
  })
})

describe('buildCreateRuleBody', () => {
  test('builds the minimal body with every optional field null', () => {
    expect(buildCreateRuleBody(baseRule)).toEqual({
      subject_kind: 'sandbox',
      subject_id: 7,
      direction: 'egress',
      peer_kind: 'cidr',
      peer_ref: '10.0.0.0/8',
      action: 'deny',
      protocol: null,
      port_start: null,
      port_end: null,
      description: null,
    })
  })

  test('carries protocol, port range and a trimmed description', () => {
    const body = buildCreateRuleBody({
      ...baseRule,
      protocol: 'udp',
      port: '8000',
      portEnd: '8100',
      description: '  metrics range  ',
    })
    expect(body.protocol).toBe('udp')
    expect(body.port_start).toBe(8000)
    expect(body.port_end).toBe(8100)
    expect(body.description).toBe('metrics range')
  })

  test('rejects --port-end without --port', () => {
    expect(() => buildCreateRuleBody({ ...baseRule, portEnd: '90' })).toThrow('--port-end needs --port')
  })

  test('rejects an inverted port range', () => {
    expect(() => buildCreateRuleBody({ ...baseRule, port: '90', portEnd: '80' })).toThrow(
      '--port-end 80 is below --port 90',
    )
  })

  test('rejects an out-of-range port', () => {
    expect(() => buildCreateRuleBody({ ...baseRule, port: '70000' })).toThrow('Invalid --port')
  })

  test('rejects an unknown direction, action or peer kind', () => {
    expect(() => buildCreateRuleBody({ ...baseRule, direction: 'outbound' })).toThrow('Invalid --direction')
    expect(() => buildCreateRuleBody({ ...baseRule, action: 'block' })).toThrow('Invalid --action')
    expect(() => buildCreateRuleBody({ ...baseRule, peerKind: 'ip' })).toThrow('Invalid --peer-kind')
  })

  test('rejects an empty peer', () => {
    expect(() => buildCreateRuleBody({ ...baseRule, peer: '   ' })).toThrow('--peer cannot be empty')
  })
})

describe('buildProfileBody', () => {
  test('omitted directions are sent as null (the kind default)', () => {
    expect(buildProfileBody({})).toEqual({ egress: null, ingress: null })
    expect(buildProfileBody({ egress: 'restricted' })).toEqual({ egress: 'restricted', ingress: null })
  })

  test('rejects an unknown profile', () => {
    expect(() => buildProfileBody({ ingress: 'closed' })).toThrow(
      "Invalid --ingress 'closed': expected one of open, restricted, none",
    )
  })
})

describe('formatPorts', () => {
  test('formats every shape', () => {
    expect(formatPorts({ port_start: null, port_end: null })).toBe('all')
    expect(formatPorts({ port_start: 443, port_end: null })).toBe('443')
    expect(formatPorts({ port_start: 443, port_end: 443 })).toBe('443')
    expect(formatPorts({ port_start: 8000, port_end: 8100 })).toBe('8000-8100')
  })
})

describe('registerNetworkPolicyCommands', () => {
  const program = new Command()
  registerNetworkPolicyCommands(program)
  const netpol = program.commands.find((c) => c.name() === 'network-policy')
  const sub = (parent: Command | undefined, name: string) =>
    parent?.commands.find((c) => c.name() === name)

  test('registers the group with its alias', () => {
    expect(netpol).toBeDefined()
    expect(netpol?.aliases()).toContain('netpol')
  })

  test('registers rules list/show/create/rm, effective and profile set', () => {
    const rules = sub(netpol, 'rules')
    expect(rules?.commands.map((c) => c.name()).sort()).toEqual(['create', 'list', 'rm', 'show'])
    expect(sub(netpol, 'effective')).toBeDefined()
    expect(sub(sub(netpol, 'profile'), 'set')).toBeDefined()
  })

  test('does not expose the node enforcement-mode switch', () => {
    expect(sub(netpol, 'nodes')).toBeUndefined()
    expect(sub(netpol, 'mode')).toBeUndefined()
  })

  test('rules create requires the rule fields and accepts the optional ones', () => {
    const create = sub(sub(netpol, 'rules'), 'create')
    const required = create?.options.filter((o) => o.mandatory).map((o) => o.long)
    expect(required?.sort()).toEqual(
      ['--action', '--direction', '--peer', '--peer-kind', '--subject-id', '--subject-kind'].sort(),
    )
    const longs = create?.options.map((o) => o.long)
    for (const flag of ['--protocol', '--port', '--port-end', '--description', '--json']) {
      expect(longs).toContain(flag)
    }
  })

  test('profile set takes --egress and --ingress', () => {
    const set = sub(sub(netpol, 'profile'), 'set')
    expect(set?.options.map((o) => o.long)).toEqual(['--egress', '--ingress', '--json'])
  })
})
