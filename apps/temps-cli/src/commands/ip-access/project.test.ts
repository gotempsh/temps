// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import { buildCidrList, parseId, parseMode, registerProjectIpAccessCommands } from './project.js'

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

describe('registration', () => {
  test('registers show, set, clear and test under ip-access project', () => {
    const program = new Command()
    const ipAccess = program.command('ip-access')
    registerProjectIpAccessCommands(ipAccess)
    const project = ipAccess.commands.find((c) => c.name() === 'project')
    expect(project?.commands.map((c) => c.name()).sort()).toEqual(['clear', 'set', 'show', 'test'])
  })
})
