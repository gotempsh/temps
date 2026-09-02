// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  isValidChallengeType,
  isValidRenewalMethod,
  readAcmeJsonFile,
} from './tls.js'

describe('isValidChallengeType', () => {
  test('accepts http-01', () => {
    expect(isValidChallengeType('http-01')).toBe(true)
  })

  test('accepts dns-01', () => {
    expect(isValidChallengeType('dns-01')).toBe(true)
  })

  test('rejects an unsupported challenge type', () => {
    // tls-alpn-01 is a real ACME challenge type, but ADR-041 only implements
    // the two above — this must not silently pass through.
    expect(isValidChallengeType('tls-alpn-01')).toBe(false)
  })

  test('rejects an empty string', () => {
    expect(isValidChallengeType('')).toBe(false)
  })

  test('is case-sensitive — no silent normalization', () => {
    expect(isValidChallengeType('HTTP-01')).toBe(false)
  })
})

describe('isValidRenewalMethod', () => {
  test('accepts http-01', () => {
    expect(isValidRenewalMethod('http-01')).toBe(true)
  })

  test('accepts dns-01', () => {
    expect(isValidRenewalMethod('dns-01')).toBe(true)
  })

  test('rejects an unsupported renewal method', () => {
    expect(isValidRenewalMethod('manual')).toBe(false)
  })

  test('rejects an empty string', () => {
    expect(isValidRenewalMethod('')).toBe(false)
  })
})

describe('readAcmeJsonFile', () => {
  test('reads the raw contents of an existing file', () => {
    const dir = mkdtempSync(join(tmpdir(), 'temps-cli-tls-test-'))
    const file = join(dir, 'acme.json')
    try {
      // Deliberately not valid Traefik acme.json shape — this function only
      // reads bytes, it never parses or validates JSON. The 8-step X.509
      // chain validation happens server-side (ADR-041 §5), so a malformed
      // document is still "read successfully" from this function's point of
      // view; the server is what rejects it.
      writeFileSync(file, '{ not actually valid json ]')

      const result = readAcmeJsonFile(file)

      expect(result).toEqual({ ok: true, contents: '{ not actually valid json ]' })
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test('a missing file produces an actionable message, not a raw ENOENT', () => {
    const dir = mkdtempSync(join(tmpdir(), 'temps-cli-tls-test-'))
    const missing = join(dir, 'does-not-exist.json')
    try {
      const result = readAcmeJsonFile(missing)

      expect(result.ok).toBe(false)
      if (!result.ok) {
        expect(result.message).toContain(missing)
        expect(result.message).toContain('Failed to read')
      }
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  test('a path that is a directory fails with a readable message', () => {
    const dir = mkdtempSync(join(tmpdir(), 'temps-cli-tls-test-'))
    try {
      const result = readAcmeJsonFile(dir)

      expect(result.ok).toBe(false)
      if (!result.ok) {
        expect(result.message).toContain(dir)
        expect(result.message).toContain('Failed to read')
      }
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })
})
