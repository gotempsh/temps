// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { originOf } from './index.js'

// originOf is the comparison primitive that decides whether a credential
// bound to one URL may be reused for another (see ensureMcpAuth /
// findContextByUrl). It must be strict: two URLs are "the same target" only
// when their scheme, host, and port all match exactly -- never a substring,
// prefix, or host-only match -- otherwise a mistyped, malicious, or
// scheme-downgraded URL could be treated as matching a saved, trusted
// context and inherit its credential.
describe('originOf', () => {
  test('extracts scheme+host+port from a URL', () => {
    expect(originOf('http://localhost:3000/mcp')).toBe('http://localhost:3000')
  })

  test('extracts scheme+host when no port is given (default port implied)', () => {
    expect(originOf('https://temps.example.com/mcp')).toBe('https://temps.example.com')
  })

  test('is insensitive to path and query string', () => {
    expect(originOf('https://temps.example.com/mcp?groups=platform&write=1')).toBe('https://temps.example.com')
    expect(originOf('https://temps.example.com')).toBe('https://temps.example.com')
  })

  test('different ports on the same hostname do not match', () => {
    expect(originOf('http://localhost:3000')).not.toBe(originOf('http://localhost:8080'))
  })

  test('a subdomain does not match its parent domain', () => {
    expect(originOf('https://evil.temps.example.com')).not.toBe(originOf('https://temps.example.com'))
  })

  test('a lookalike host is not treated as the same as the real one', () => {
    expect(originOf('https://temps.example.com.evil.net')).not.toBe(originOf('https://temps.example.com'))
  })

  test('http and https on the same host do NOT match (scheme downgrade must not inherit a credential)', () => {
    expect(originOf('http://temps.example.com')).not.toBe(originOf('https://temps.example.com'))
  })

  test('returns null for an unparsable URL rather than throwing', () => {
    expect(originOf('not a url')).toBeNull()
  })
})
