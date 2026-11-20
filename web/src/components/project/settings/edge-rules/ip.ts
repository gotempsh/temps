// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// IPv4/IPv6 address and CIDR parsing for edge rule validation and the
// in-browser rule tester.

export interface ParsedIp {
  version: 4 | 6
  value: bigint
}

export interface ParsedCidr extends ParsedIp {
  prefix: number
}

function parseIPv4(s: string): bigint | null {
  const parts = s.split('.')
  if (parts.length !== 4) return null
  let value = 0n
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part)) return null
    const n = Number(part)
    if (n > 255) return null
    value = (value << 8n) | BigInt(n)
  }
  return value
}

function parseIPv6(input: string): bigint | null {
  let s = input.split('%')[0]
  const lastColon = s.lastIndexOf(':')
  if (lastColon === -1) return null
  const tail = s.slice(lastColon + 1)
  if (tail.includes('.')) {
    const v4 = parseIPv4(tail)
    if (v4 === null) return null
    const hi = (v4 >> 16n) & 0xffffn
    const lo = v4 & 0xffffn
    s = `${s.slice(0, lastColon + 1)}${hi.toString(16)}:${lo.toString(16)}`
  }
  const halves = s.split('::')
  if (halves.length > 2) return null
  const head = halves[0] ? halves[0].split(':') : []
  const rest = halves.length === 2 && halves[1] ? halves[1].split(':') : []
  let groups: string[]
  if (halves.length === 1) {
    if (head.length !== 8) return null
    groups = head
  } else {
    const missing = 8 - head.length - rest.length
    if (missing < 1) return null
    groups = [...head, ...Array<string>(missing).fill('0'), ...rest]
  }
  let value = 0n
  for (const group of groups) {
    if (!/^[0-9a-f]{1,4}$/i.test(group)) return null
    value = (value << 16n) | BigInt(parseInt(group, 16))
  }
  return value
}

/** Parses an address. IPv4-mapped IPv6 (`::ffff:a.b.c.d`) is reported as
 * IPv4, matching the proxy's client IP normalization. */
export function parseIp(raw: string): ParsedIp | null {
  const s = raw.trim().replace(/^\[|\]$/g, '')
  if (!s) return null
  if (s.includes(':')) {
    const value = parseIPv6(s)
    if (value === null) return null
    if (value >> 32n === 0xffffn)
      return { version: 4, value: value & 0xffffffffn }
    return { version: 6, value }
  }
  const value = parseIPv4(s)
  return value === null ? null : { version: 4, value }
}

export function parseCidr(raw: string): ParsedCidr | null {
  const [addr, prefixText, extra] = raw.trim().split('/')
  if (extra !== undefined) return null
  const ip = parseIp(addr)
  if (!ip) return null
  const bits = ip.version === 4 ? 32 : 128
  if (prefixText === undefined) return { ...ip, prefix: bits }
  if (!/^\d{1,3}$/.test(prefixText)) return null
  const prefix = Number(prefixText)
  if (prefix > bits) return null
  return { ...ip, prefix }
}

export function cidrContains(cidr: ParsedCidr, ip: ParsedIp): boolean {
  if (cidr.version !== ip.version) return false
  const bits = BigInt(cidr.version === 4 ? 32 : 128)
  const hostBits = bits - BigInt(cidr.prefix)
  const mask = ((1n << bits) - 1n) ^ ((1n << hostBits) - 1n)
  return (cidr.value & mask) === (ip.value & mask)
}

/** An AS number with or without the `AS` prefix, or null; 32 bits at most. */
export function parseAsn(raw: string): number | null {
  const digits = raw.trim().replace(/^AS/i, '')
  if (!/^\d{1,10}$/.test(digits)) return null
  const n = Number(digits)
  return n <= 4294967295 ? n : null
}
