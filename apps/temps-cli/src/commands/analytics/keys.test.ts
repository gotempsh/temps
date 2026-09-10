// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import {
  buildIngestKeyUpdateBody,
  parseOrigins,
  formatOrigins,
  formatRateLimit,
  formatScope,
} from './keys.js'

describe('buildIngestKeyUpdateBody (three-state PATCH semantics)', () => {
  test('an omitted flag leaves the field absent, not undefined', () => {
    const result = buildIngestKeyUpdateBody({ name: 'renamed' })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(Object.keys(result.body)).toEqual(['name'])
    expect('allowed_origins' in result.body).toBe(false)
    expect('rate_limit_per_minute' in result.body).toBe(false)
  })

  test('--clear-origins sends an explicit null', () => {
    const result = buildIngestKeyUpdateBody({ clearOrigins: true })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.body.allowed_origins).toBeNull()
  })

  test('--clear-rate-limit sends an explicit null', () => {
    const result = buildIngestKeyUpdateBody({ clearRateLimit: true })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.body.rate_limit_per_minute).toBeNull()
  })

  test('values replace the stored ones', () => {
    const result = buildIngestKeyUpdateBody({
      allowedOrigins: ['https://example.com'],
      rateLimit: '1200',
    })
    expect(result.ok).toBe(true)
    if (!result.ok) return
    expect(result.body.allowed_origins).toEqual(['https://example.com'])
    expect(result.body.rate_limit_per_minute).toBe(1200)
  })

  test('rejects a flag and its clear counterpart together', () => {
    expect(
      buildIngestKeyUpdateBody({
        allowedOrigins: ['https://example.com'],
        clearOrigins: true,
      }).ok,
    ).toBe(false)
    expect(
      buildIngestKeyUpdateBody({ rateLimit: '10', clearRateLimit: true }).ok,
    ).toBe(false)
  })

  test('rejects a non-numeric rate limit instead of sending NaN', () => {
    const result = buildIngestKeyUpdateBody({ rateLimit: 'lots' })
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('--rate-limit')
  })

  test('rejects an update that would change nothing', () => {
    const result = buildIngestKeyUpdateBody({})
    expect(result.ok).toBe(false)
    if (result.ok) return
    expect(result.error).toContain('Nothing to update')
  })

  test('rejects an empty name rather than blanking the label', () => {
    expect(buildIngestKeyUpdateBody({ name: '   ' }).ok).toBe(false)
  })
})

describe('parseOrigins', () => {
  test('accepts repeated values and comma-separated lists', () => {
    expect(parseOrigins(['https://a.com', 'https://b.com,https://c.com'])).toEqual([
      'https://a.com',
      'https://b.com',
      'https://c.com',
    ])
  })

  test('drops blanks and trims whitespace', () => {
    expect(parseOrigins([' https://a.com , ', ''])).toEqual(['https://a.com'])
  })
})

describe('display formatting', () => {
  test('no origin allowlist means any origin', () => {
    expect(formatOrigins(null)).toBe('any origin')
    expect(formatOrigins([])).toBe('any origin')
    expect(formatOrigins(['https://a.com'])).toBe('https://a.com')
  })

  test('null or non-positive rate limits are unlimited', () => {
    expect(formatRateLimit(null)).toBe('unlimited')
    expect(formatRateLimit(0)).toBe('unlimited')
    expect(formatRateLimit(-1)).toBe('unlimited')
    expect(formatRateLimit(600)).toBe('600/min')
  })

  test('a key without an environment is project-wide', () => {
    expect(formatScope(null)).toBe('project-wide')
    expect(formatScope(12)).toBe('12')
  })
})
