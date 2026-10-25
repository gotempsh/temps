// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  backendEnforcesRestrictedEgress,
  describeRule,
  draftsFromPolicy,
  looksLikeCidr,
  parseCidrList,
  parsePorts,
  ruleProblem,
  rulesFromDrafts,
} from './sandbox-network'

describe('preview allow-list', () => {
  test('accepts addresses and CIDRs of both families', () => {
    for (const value of [
      '203.0.113.7',
      '198.51.100.0/24',
      '0.0.0.0/0',
      '2001:db8::1',
      '2606:4700::/32',
    ]) {
      expect(looksLikeCidr(value)).toBe(true)
    }
  })

  test('rejects hostnames, bad octets and out-of-range prefixes', () => {
    for (const value of [
      'example.com',
      '256.1.1.1',
      '10.0.0.0/33',
      '2001:db8::/129',
      '1.2.3',
      '1.2.3.4/x',
      '',
    ]) {
      expect(looksLikeCidr(value)).toBe(false)
    }
  })

  test('reads one entry per line or comma and names the first bad one', () => {
    expect(
      parseCidrList('203.0.113.7\n\n 198.51.100.0/24 , 2001:db8::1')
    ).toEqual({
      entries: ['203.0.113.7', '198.51.100.0/24', '2001:db8::1'],
      invalid: null,
    })
    expect(parseCidrList('203.0.113.7\nmy-office').invalid).toBe('my-office')
    expect(parseCidrList('   ')).toEqual({ entries: [], invalid: null })
  })
})

describe('egress rules', () => {
  test('ports: empty means the defaults, duplicates collapse, junk is refused', () => {
    expect(parsePorts('')).toEqual([])
    expect(parsePorts('443, 8443 443')).toEqual([443, 8443])
    for (const bad of ['0', '65536', 'http', '44.3', '-1']) {
      expect(parsePorts(bad)).toBeNull()
    }
  })

  test('explains what is wrong with a row before it is sent', () => {
    expect(
      ruleProblem({ kind: 'host', value: 'api.example.com', ports: '' })
    ).toBeNull()
    expect(
      ruleProblem({ kind: 'host', value: '*.example.com', ports: '443' })
    ).toBeNull()
    expect(
      ruleProblem({ kind: 'cidr', value: '198.51.100.0/24', ports: '' })
    ).toBeNull()
    expect(ruleProblem({ kind: 'host', value: '', ports: '' })).toBe(
      'Enter a hostname'
    )
    expect(
      ruleProblem({ kind: 'host', value: 'https://api.example.com', ports: '' })
    ).toContain('not a hostname')
    expect(
      ruleProblem({ kind: 'host', value: '1.1.1.1', ports: '' })
    ).toContain('not a hostname')
    expect(
      ruleProblem({ kind: 'cidr', value: 'example.com', ports: '' })
    ).toContain('not an IP address')
    expect(
      ruleProblem({ kind: 'host', value: 'a.example.com', ports: '22,x' })
    ).toContain('Ports are numbers')
  })

  test('round-trips the server policy through the editor', () => {
    const policy = {
      mode: 'restricted' as const,
      rules: [
        { host: 'api.example.com', ports: [80, 443] },
        { cidr: '198.51.100.0/24', ports: [8443] },
      ],
    }
    const drafts = draftsFromPolicy(policy)
    expect(drafts).toEqual([
      { kind: 'host', value: 'api.example.com', ports: '80, 443' },
      { kind: 'cidr', value: '198.51.100.0/24', ports: '8443' },
    ])
    expect(rulesFromDrafts(drafts)).toEqual(policy.rules)
    expect(
      rulesFromDrafts([{ kind: 'host', value: ' API.Example.com ', ports: '' }])
    ).toEqual([{ host: 'api.example.com', ports: [] }])
  })

  test('describes a rule with its effective ports', () => {
    expect(describeRule({ host: 'api.example.com' })).toBe(
      'api.example.com on 80, 443'
    )
    expect(describeRule({ cidr: '198.51.100.0/24', ports: [22] })).toBe(
      '198.51.100.0/24 on 22'
    )
  })

  test('only the docker backend enforces a restricted policy', () => {
    expect(backendEnforcesRestrictedEgress('docker')).toBe(true)
    expect(backendEnforcesRestrictedEgress(undefined)).toBe(true)
    for (const backend of ['firecracker', 'microsandbox', 'local']) {
      expect(backendEnforcesRestrictedEgress(backend)).toBe(false)
    }
  })
})
