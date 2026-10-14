// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  MAX_CIDRS,
  TAILSCALE_PRESETS,
  cidrSyntaxError,
  hasAllPresets,
  hostCidr,
  initialFormMode,
  ipAccessFormSchema,
  showsEntries,
  policySummary,
  withPresets,
} from './ip-access'

describe('cidrSyntaxError', () => {
  test('accepts IPv4 and IPv6 addresses and ranges', () => {
    for (const value of [
      '203.0.113.9',
      '203.0.113.0/24',
      '0.0.0.0/0',
      '100.64.0.0/10',
      '2001:db8::/32',
      'fd7a:115c:a1e0::/48',
      '::1',
      '::/0',
      '::ffff:192.0.2.1',
      '2001:db8::1/128',
    ]) {
      expect(cidrSyntaxError(value)).toBeNull()
    }
  })

  test('rejects malformed input with a reason', () => {
    expect(cidrSyntaxError('')).not.toBeNull()
    expect(cidrSyntaxError('   ')).not.toBeNull()
    expect(cidrSyntaxError('example.test')).not.toBeNull()
    expect(cidrSyntaxError('256.0.0.1')).not.toBeNull()
    expect(cidrSyntaxError('10.0.0.0/33')).toContain('32')
    expect(cidrSyntaxError('2001:db8::/129')).toContain('128')
    expect(cidrSyntaxError('10.0.0.0/')).not.toBeNull()
    expect(cidrSyntaxError('10.0.0.0/8/8')).not.toBeNull()
    expect(cidrSyntaxError('1:2:3:4:5:6:7')).not.toBeNull()
    expect(cidrSyntaxError('1::2::3')).not.toBeNull()
  })

  test('trims surrounding whitespace', () => {
    expect(cidrSyntaxError('  203.0.113.0/24 ')).toBeNull()
  })
})

describe('ipAccessFormSchema', () => {
  test('rejects an empty allow list', () => {
    const result = ipAccessFormSchema.safeParse({ mode: 'allow', cidrs: [] })
    expect(result.success).toBe(false)
  })

  test('accepts an empty deny list and a disabled list', () => {
    expect(
      ipAccessFormSchema.safeParse({ mode: 'deny', cidrs: [] }).success
    ).toBe(true)
    expect(
      ipAccessFormSchema.safeParse({ mode: 'disabled', cidrs: [] }).success
    ).toBe(true)
  })

  test('rejects duplicates regardless of case and spacing', () => {
    const result = ipAccessFormSchema.safeParse({
      mode: 'deny',
      cidrs: [{ cidr: '2001:DB8::/32' }, { cidr: ' 2001:db8::/32 ' }],
    })
    expect(result.success).toBe(false)
  })

  test('enforces the entry limit', () => {
    const cidrs = Array.from({ length: MAX_CIDRS + 1 }, (_, i) => ({
      cidr: `10.${Math.floor(i / 256)}.${i % 256}.0/24`,
    }))
    expect(ipAccessFormSchema.safeParse({ mode: 'deny', cidrs }).success).toBe(
      false
    )
  })

  test('reports the offending row path', () => {
    const result = ipAccessFormSchema.safeParse({
      mode: 'deny',
      cidrs: [{ cidr: '203.0.113.0/24' }, { cidr: 'nope' }],
    })
    expect(result.success).toBe(false)
    if (!result.success) {
      expect(result.error.issues[0].path).toEqual(['cidrs', 1, 'cidr'])
    }
  })
})

describe('presets', () => {
  test('adds both Tailscale ranges once', () => {
    const once = withPresets([], TAILSCALE_PRESETS)
    expect(once.map((entry) => entry.cidr)).toEqual([
      '100.64.0.0/10',
      'fd7a:115c:a1e0::/48',
    ])
    expect(withPresets(once, TAILSCALE_PRESETS)).toHaveLength(2)
    expect(hasAllPresets(once, TAILSCALE_PRESETS)).toBe(true)
  })

  test('keeps existing entries and only adds what is missing', () => {
    const merged = withPresets(
      [{ cidr: '100.64.0.0/10' }, { cidr: '203.0.113.0/24' }],
      TAILSCALE_PRESETS
    )
    expect(merged.map((entry) => entry.cidr)).toEqual([
      '100.64.0.0/10',
      '203.0.113.0/24',
      'fd7a:115c:a1e0::/48',
    ])
  })
})

describe('scope modes', () => {
  test('an environment without its own list starts by following the project', () => {
    const unconfigured = { configured: false, mode: 'disabled' as const }
    expect(initialFormMode(unconfigured, true)).toBe('inherit')
    expect(initialFormMode(unconfigured, false)).toBe('disabled')
  })

  test('a saved environment override keeps its mode, including off', () => {
    expect(initialFormMode({ configured: true, mode: 'disabled' }, true)).toBe(
      'disabled'
    )
    expect(initialFormMode({ configured: true, mode: 'allow' }, true)).toBe(
      'allow'
    )
  })

  test('inherit is a valid form value with no entries and hides the editor', () => {
    expect(
      ipAccessFormSchema.safeParse({ mode: 'inherit', cidrs: [] }).success
    ).toBe(true)
    expect(showsEntries('inherit')).toBe(false)
    expect(showsEntries('disabled')).toBe(true)
  })
})

describe('helpers', () => {
  test('hostCidr uses the single-host prefix for each family', () => {
    expect(hostCidr('203.0.113.9')).toBe('203.0.113.9/32')
    expect(hostCidr('2001:db8::9')).toBe('2001:db8::9/128')
  })

  test('policySummary pluralises', () => {
    expect(policySummary('allow', 1)).toBe('Only 1 range can reach this')
    expect(policySummary('deny', 3)).toBe('3 ranges blocked')
    expect(policySummary('disabled', 0)).toBe('Open to everyone')
  })
})
