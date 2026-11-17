// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  previewLinkBlockedReason,
  previewLinkExpiry,
  previewLinkFormSchema,
  toPreviewLinkBody,
} from './sandbox-preview-links'

describe('previewLinkFormSchema', () => {
  const parse = (value: { port: string; path: string; ttlSeconds: string }) =>
    previewLinkFormSchema.safeParse(value).success

  test('accepts a port, an optional path and a lifetime', () => {
    expect(parse({ port: '3000', path: '', ttlSeconds: '3600' })).toBe(true)
    expect(parse({ port: '8080', path: '/docs', ttlSeconds: '900' })).toBe(true)
  })

  test('rejects an out-of-range port and an open-redirect path', () => {
    expect(parse({ port: '0', path: '', ttlSeconds: '3600' })).toBe(false)
    expect(parse({ port: '65536', path: '', ttlSeconds: '3600' })).toBe(false)
    expect(parse({ port: 'web', path: '', ttlSeconds: '3600' })).toBe(false)
    expect(
      parse({ port: '3000', path: '//evil.test', ttlSeconds: '3600' })
    ).toBe(false)
    expect(parse({ port: '3000', path: 'docs', ttlSeconds: '3600' })).toBe(
      false
    )
  })
})

describe('toPreviewLinkBody', () => {
  test('omits an empty path and converts numbers', () => {
    expect(
      toPreviewLinkBody({ port: '3000', path: '', ttlSeconds: '3600' })
    ).toEqual({ port: 3000, ttl_seconds: 3600 })
    expect(
      toPreviewLinkBody({ port: '5173', path: ' /app ', ttlSeconds: '900' })
    ).toEqual({ port: 5173, path: '/app', ttl_seconds: 900 })
  })
})

describe('previewLinkBlockedReason', () => {
  const ok = {
    configured: true,
    hasPassword: true,
    destroyed: false,
    running: true,
  }
  test('is null when a link can be created', () => {
    expect(previewLinkBlockedReason(ok)).toBeNull()
  })
  test('names the missing prerequisite', () => {
    expect(previewLinkBlockedReason({ ...ok, hasPassword: false })).toContain(
      'preview password'
    )
    expect(previewLinkBlockedReason({ ...ok, running: false })).toContain(
      'not running'
    )
    expect(previewLinkBlockedReason({ ...ok, destroyed: true })).toContain(
      'destroyed'
    )
    expect(previewLinkBlockedReason({ ...ok, configured: false })).toContain(
      'not configured'
    )
  })
})

describe('previewLinkExpiry', () => {
  // 2026-01-01T12:30:00Z
  const expiresAt = Date.UTC(2026, 0, 1, 12, 30) / 1000

  test('is an absolute wall-clock time that does not change as time passes', () => {
    const options = { locale: 'en-US', timeZone: 'UTC' }
    expect(previewLinkExpiry(expiresAt, options)).toBe(
      'expires Jan 1, 12:30 PM'
    )
    // Same input, same output: there is no "now" to go stale against.
    expect(previewLinkExpiry(expiresAt, options)).toBe(
      previewLinkExpiry(expiresAt, options)
    )
  })

  test('is rendered in the requested time zone', () => {
    expect(
      previewLinkExpiry(expiresAt, { locale: 'en-US', timeZone: 'Asia/Tokyo' })
    ).toBe('expires Jan 1, 9:30 PM')
  })
})
