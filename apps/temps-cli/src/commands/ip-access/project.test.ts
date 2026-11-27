// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import {
  buildCidrList,
  monitorBlockLines,
  parseId,
  parseMode,
  registerProjectIpAccessCommands,
  resolveSetInput,
} from './project.js'

describe('parseMode', () => {
  test('accepts the three modes', () => {
    expect(parseMode('allow')).toBe('allow')
    expect(parseMode('deny')).toBe('deny')
    expect(parseMode('disabled')).toBe('disabled')
  })

  test('rejects anything else, including the instance-wide "block"', () => {
    expect(parseMode('block')).toBeUndefined()
    expect(parseMode('')).toBeUndefined()
    expect(parseMode('Allow')).toBeUndefined()
  })
})

describe('parseId', () => {
  test('accepts positive integers', () => {
    expect(parseId('12', '--project-id')).toBe(12)
  })

  test('names the offending flag', () => {
    expect(() => parseId('abc', '--project-id')).toThrow('--project-id')
    expect(() => parseId('0', '--environment-id')).toThrow('--environment-id')
    expect(() => parseId(undefined, '--project-id')).toThrow('--project-id')
    expect(() => parseId('1.5', '--project-id')).toThrow()
  })
})

describe('buildCidrList', () => {
  test('splits comma-separated values and trims', () => {
    expect(buildCidrList(['203.0.113.0/24, 2001:db8::/32'], false).map((e) => e.cidr)).toEqual([
      '203.0.113.0/24',
      '2001:db8::/32',
    ])
  })

  test('adds Tailscale ranges once even when already listed', () => {
    const list = buildCidrList(['100.64.0.0/10'], true)
    expect(list.map((e) => e.cidr)).toEqual(['100.64.0.0/10', 'fd7a:115c:a1e0::/48'])
  })

  test('drops blanks and case-insensitive duplicates', () => {
    expect(buildCidrList(['', '2001:DB8::/32', '2001:db8::/32'], false)).toHaveLength(1)
  })

  test('is empty when nothing is given', () => {
    expect(buildCidrList([], false)).toEqual([])
  })
})

describe('resolveSetInput', () => {
  test('returns the mode and list for valid input', () => {
    const input = resolveSetInput({ project: '1', mode: 'deny', cidr: ['203.0.113.0/24'] } as never)
    expect(input.mode).toBe('deny')
    expect(input.cidrs.map((e) => e.cidr)).toEqual(['203.0.113.0/24'])
  })

  test('throws on an unknown mode instead of returning quietly', () => {
    expect(() => resolveSetInput({ project: '1', mode: 'block', cidr: ['203.0.113.0/24'] } as never)).toThrow(
      'Invalid mode: block'
    )
  })

  test('throws on an allow list with no entries', () => {
    expect(() => resolveSetInput({ project: '1', mode: 'allow' } as never)).toThrow('at least one --cidr')
  })

  test('accepts an allow list made of the Tailscale ranges alone', () => {
    expect(resolveSetInput({ project: '1', mode: 'allow', tailscale: true } as never).cidrs).toHaveLength(2)
  })

  test('accepts an empty deny or disabled list', () => {
    expect(resolveSetInput({ project: '1', mode: 'deny' } as never).cidrs).toEqual([])
    expect(resolveSetInput({ project: '1', mode: 'disabled' } as never).mode).toBe('disabled')
  })
})

describe('registration', () => {
  test('registers show, set, clear and test under ip-access project', () => {
    const program = new Command()
    const ipAccess = program.command('ip-access')
    registerProjectIpAccessCommands(ipAccess)
    const project = ipAccess.commands.find((c) => c.name() === 'project')
    expect(project?.commands.map((c) => c.name()).sort()).toEqual(['clear', 'set', 'show', 'test'])
  })
})

describe('monitorBlockLines', () => {
  const block = {
    project_id: 4,
    environment_id: 9,
    policy_scope: 'project' as const,
    mode: 'allow' as const,
    reason: 'not_allow_listed' as const,
    monitor_addresses: ['127.0.0.1', '203.0.113.10'],
    blocked_addresses: ['203.0.113.10'],
    matched_cidrs: [],
    suggested_cidrs: ['203.0.113.10/32'],
    suggested_description: 'Temps uptime monitor',
    message: 'Blocked by this project\'s IP access list: ...',
  }

  test('prints the reason and a set command that keeps the saved entries', () => {
    const lines = monitorBlockLines(block, {
      mode: 'allow',
      cidrs: [{ cidr: '198.51.100.7/32' }, { cidr: '127.0.0.1/32' }],
      isEnvironment: false,
    })
    expect(lines).toEqual([
      block.message,
      'To allow it: temps ip-access project set --project-id 4 --mode allow --cidr 198.51.100.7/32 --cidr 127.0.0.1/32 --cidr 203.0.113.10/32',
    ])
  })

  test('points an inheriting environment at the project list', () => {
    const lines = monitorBlockLines(block, { mode: 'disabled', cidrs: [], isEnvironment: true })
    expect(lines[1]).toBe('Fix it on the project list: temps ip-access project show --project-id 4')
  })

  test('a deny-list block has nothing to add', () => {
    const lines = monitorBlockLines(
      { ...block, mode: 'deny', reason: 'deny_listed', suggested_cidrs: [], matched_cidrs: ['127.0.0.0/8'] },
      { mode: 'deny', cidrs: [{ cidr: '127.0.0.0/8' }], isEnvironment: false },
    )
    expect(lines).toEqual([block.message])
  })
})
