// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { cidrContains, parseCidr, parseIp } from './ip'

const contains = (cidr: string, ip: string) => {
  const c = parseCidr(cidr)
  const i = parseIp(ip)
  if (!c || !i) throw new Error(`unparseable ${cidr} / ${ip}`)
  return cidrContains(c, i)
}

describe('edge rule IP parsing', () => {
  test('parses IPv4 and rejects malformed addresses', () => {
    expect(parseIp('203.0.113.7')?.version).toBe(4)
    for (const bad of ['256.0.0.1', '1.2.3', '1.2.3.4.5', '01234.1.1.1', '']) {
      expect(parseIp(bad)).toBeNull()
    }
  })

  test('parses compressed, full and bracketed IPv6', () => {
    expect(parseIp('::1')?.value).toBe(1n)
    expect(parseIp('::')?.value).toBe(0n)
    expect(parseIp('2001:db8::1')?.version).toBe(6)
    expect(parseIp('[2001:db8::1]')?.version).toBe(6)
    expect(parseIp('2001:0db8:0000:0000:0000:0000:0000:0001')?.value).toBe(
      parseIp('2001:db8::1')?.value
    )
    for (const bad of ['2001::db8::1', '1:2:3:4:5:6:7:8:9', 'gggg::1', ':::']) {
      expect(parseIp(bad)).toBeNull()
    }
  })

  test('treats IPv4-mapped IPv6 as IPv4, like the proxy does', () => {
    const mapped = parseIp('::ffff:203.0.113.7')
    expect(mapped?.version).toBe(4)
    expect(contains('203.0.113.0/24', '::ffff:203.0.113.7')).toBe(true)
  })

  test('matches CIDR ranges of both families', () => {
    expect(contains('203.0.113.0/24', '203.0.113.255')).toBe(true)
    expect(contains('203.0.113.0/24', '203.0.114.0')).toBe(false)
    expect(contains('198.51.100.7', '198.51.100.7')).toBe(true)
    expect(contains('0.0.0.0/0', '8.8.8.8')).toBe(true)
    expect(contains('2001:db8::/32', '2001:db8:ffff::1')).toBe(true)
    expect(contains('2001:db8::/32', '2001:db9::1')).toBe(false)
  })

  test('never matches across address families', () => {
    expect(contains('0.0.0.0/0', '2001:db8::1')).toBe(false)
    expect(contains('::/0', '8.8.8.8')).toBe(false)
  })

  test('rejects invalid prefixes', () => {
    expect(parseCidr('10.0.0.0/33')).toBeNull()
    expect(parseCidr('2001:db8::/129')).toBeNull()
    expect(parseCidr('10.0.0.0/8/8')).toBeNull()
    expect(parseCidr('10.0.0.0/x')).toBeNull()
  })
})
