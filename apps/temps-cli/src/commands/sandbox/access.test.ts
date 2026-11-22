// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { Command } from 'commander'
import {
  buildEgressPolicy,
  buildPreviewAccessBody,
  describeEgressPolicy,
  describePreviewAccess,
  parseEgressRule,
  parsePreviewAccess,
  wantsEgressChange,
  wantsPreviewAccessChange,
} from './access.js'
import { registerSandboxCommands } from './index.js'

describe('parsePreviewAccess', () => {
  it('accepts the API values and the login shorthand', () => {
    expect(parsePreviewAccess('temps_login')).toBe('temps_login')
    expect(parsePreviewAccess('temps-login')).toBe('temps_login')
    expect(parsePreviewAccess('Login')).toBe('temps_login')
    expect(parsePreviewAccess('password')).toBe('password')
    expect(parsePreviewAccess(' public ')).toBe('public')
  })

  it('rejects anything else with the valid values', () => {
    expect(() => parsePreviewAccess('anyone')).toThrow(/temps_login, password, public/)
  })
})

describe('buildPreviewAccessBody', () => {
  const current = { access: 'password' as const, allowedCidrs: ['10.0.0.0/8'] }

  it('keeps the allow-list when only the mode changes', () => {
    expect(buildPreviewAccessBody({ access: 'public' }, current)).toEqual({
      access: 'public',
      allowed_cidrs: ['10.0.0.0/8'],
    })
  })

  it('keeps the mode when only the allow-list changes, replacing the list', () => {
    expect(
      buildPreviewAccessBody({ allowIp: ['198.51.100.7', ' 2001:db8::/32 '] }, current),
    ).toEqual({ access: 'password', allowed_cidrs: ['198.51.100.7', '2001:db8::/32'] })
  })

  it('clears the allow-list with --any-ip', () => {
    expect(buildPreviewAccessBody({ anyIp: true }, current)).toEqual({
      access: 'password',
      allowed_cidrs: [],
    })
  })

  it('refuses --any-ip together with --allow-ip', () => {
    expect(() =>
      buildPreviewAccessBody({ anyIp: true, allowIp: ['10.0.0.1'] }, current),
    ).toThrow(/cannot be combined/)
  })

  it('validates the mode before building', () => {
    expect(() => buildPreviewAccessBody({ access: 'open' }, current)).toThrow(/Unknown --access/)
  })
})

describe('wantsPreviewAccessChange', () => {
  it('is false with no setting flags, so the command shows the current value', () => {
    expect(wantsPreviewAccessChange({})).toBe(false)
    expect(wantsPreviewAccessChange({ allowIp: [] })).toBe(false)
    expect(wantsPreviewAccessChange({ anyIp: true })).toBe(true)
  })
})

describe('describePreviewAccess', () => {
  it('names who can open previews and the networks', () => {
    expect(describePreviewAccess('public', [])).toBe('anyone with the URL')
    expect(describePreviewAccess('temps_login', ['10.0.0.0/8'])).toBe(
      'signed-in Temps users with access to the sandbox, from 10.0.0.0/8 only',
    )
  })
})

describe('parseEgressRule', () => {
  it('parses hostnames, wildcards and ports', () => {
    expect(parseEgressRule('API.example.com')).toEqual({ host: 'api.example.com', ports: [] })
    expect(parseEgressRule('*.example.com:443,8443')).toEqual({
      host: '*.example.com',
      ports: [443, 8443],
    })
  })

  it('parses IPv4 addresses and CIDRs as cidr rules', () => {
    expect(parseEgressRule('1.1.1.0/24')).toEqual({ cidr: '1.1.1.0/24', ports: [] })
    expect(parseEgressRule('203.0.113.5:5432')).toEqual({ cidr: '203.0.113.5', ports: [5432] })
  })

  it('treats a bare IPv6 value as an address with no ports', () => {
    expect(parseEgressRule('2606:4700::/32')).toEqual({ cidr: '2606:4700::/32', ports: [] })
  })

  it('takes IPv6 ports from the bracketed form', () => {
    expect(parseEgressRule('[2606:4700::/32]:443')).toEqual({
      cidr: '2606:4700::/32',
      ports: [443],
    })
    expect(parseEgressRule('[2001:db8::1]')).toEqual({ cidr: '2001:db8::1', ports: [] })
  })

  it('rejects bad ports and malformed values with the offending input', () => {
    expect(() => parseEgressRule('example.com:0')).toThrow(/Invalid port '0' in 'example.com:0'/)
    expect(() => parseEgressRule('example.com:https')).toThrow(/Invalid port/)
    expect(() => parseEgressRule('example.com:70000')).toThrow(/1-65535/)
    expect(() => parseEgressRule('[2001:db8::1')).toThrow(/Unclosed/)
    expect(() => parseEgressRule(':443')).toThrow(/Missing host or CIDR/)
    expect(() => parseEgressRule('  ')).toThrow(/Empty/)
  })
})

describe('buildEgressPolicy', () => {
  it('builds each intent', () => {
    expect(buildEgressPolicy({ internet: true })).toEqual({ mode: 'internet', rules: [] })
    expect(buildEgressPolicy({ denyAll: true })).toEqual({ mode: 'restricted', rules: [] })
    expect(buildEgressPolicy({ allow: ['registry.npmjs.org', '203.0.113.0/24:5432'] })).toEqual({
      mode: 'restricted',
      rules: [
        { host: 'registry.npmjs.org', ports: [] },
        { cidr: '203.0.113.0/24', ports: [5432] },
      ],
    })
  })

  it('requires exactly one intent', () => {
    expect(() => buildEgressPolicy({})).toThrow(/Pass --internet, --deny-all/)
    expect(() => buildEgressPolicy({ internet: true, allow: ['example.com'] })).toThrow(
      /mutually exclusive/,
    )
    expect(() => buildEgressPolicy({ internet: true, denyAll: true })).toThrow(
      /mutually exclusive/,
    )
  })

  it('is only built when a setting flag was given', () => {
    expect(wantsEgressChange({})).toBe(false)
    expect(wantsEgressChange({ allow: [] })).toBe(false)
    expect(wantsEgressChange({ denyAll: true })).toBe(true)
  })
})

describe('describeEgressPolicy', () => {
  it('summarises each mode', () => {
    expect(describeEgressPolicy({ mode: 'internet', rules: [] })).toBe('internet (no restriction)')
    expect(describeEgressPolicy({ mode: 'restricted', rules: [] })).toBe(
      'restricted: no outbound destinations allowed',
    )
    expect(describeEgressPolicy({ mode: 'restricted', rules: [{ host: 'a.example.com' }] })).toBe(
      'restricted: 1 allowed destination',
    )
  })
})

describe('sandbox command tree', () => {
  const program = new Command()
  registerSandboxCommands(program)
  const sandbox = program.commands.find((command) => command.name() === 'sandbox')
  const sub = (name: string) => sandbox?.commands.find((command) => command.name() === name)
  const flags = (name: string) => sub(name)?.options.map((option) => option.long) ?? []

  it('registers preview-access and egress with their flags', () => {
    expect(flags('preview-access')).toEqual(['--access', '--allow-ip', '--any-ip', '--json'])
    expect(flags('egress')).toEqual(['--internet', '--deny-all', '--allow', '--json'])
  })

  it('lets create set an egress policy', () => {
    expect(flags('create')).toContain('--egress-allow')
    expect(flags('create')).toContain('--egress-deny-all')
  })
})
