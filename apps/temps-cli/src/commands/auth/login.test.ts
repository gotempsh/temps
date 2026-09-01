// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe, beforeEach, afterEach } from 'bun:test'
import {
  debugEnabled,
  serverBaseUrl,
  absoluteVerificationUri,
  type DeviceStartResponse,
} from './login.js'

describe('serverBaseUrl', () => {
  test('adds /api to a bare host', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es')).toBe('https://app.temps.kfs.es/api')
  })

  test('leaves an already-/api-suffixed URL as-is', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es/api')).toBe('https://app.temps.kfs.es/api')
  })

  test('strips a trailing slash before adding /api', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es/')).toBe('https://app.temps.kfs.es/api')
  })

  test('strips multiple trailing slashes', () => {
    expect(serverBaseUrl('https://app.temps.kfs.es///')).toBe('https://app.temps.kfs.es/api')
  })
})

describe('debugEnabled', () => {
  const originalEnv = process.env.TEMPS_DEBUG

  beforeEach(() => {
    delete process.env.TEMPS_DEBUG
  })

  afterEach(() => {
    if (originalEnv === undefined) delete process.env.TEMPS_DEBUG
    else process.env.TEMPS_DEBUG = originalEnv
  })

  test('true when --debug is passed', () => {
    expect(debugEnabled({ debug: true })).toBe(true)
  })

  test('false with no flag and no env var', () => {
    expect(debugEnabled({})).toBe(false)
    expect(debugEnabled()).toBe(false)
  })

  test.each(['1', 'true', 'yes'])('TEMPS_DEBUG=%s enables debug logging', (value) => {
    process.env.TEMPS_DEBUG = value
    expect(debugEnabled({})).toBe(true)
  })

  test('an unrecognized TEMPS_DEBUG value does not enable debug logging', () => {
    process.env.TEMPS_DEBUG = 'verbose'
    expect(debugEnabled({})).toBe(false)
  })
})

describe('absoluteVerificationUri', () => {
  const start: DeviceStartResponse = {
    device_code: 'dc_123',
    user_code: 'ABCD-1234',
    verification_uri: '/cli-login',
    verification_uri_complete: '/cli-login/ABCD-1234',
    expires_in: 900,
    interval: 2,
  }

  test('returns an already-absolute URI unchanged', () => {
    const absolute = { ...start, verification_uri_complete: 'https://app.temps.kfs.es/cli-login/CODE' }
    expect(absoluteVerificationUri('https://app.temps.kfs.es', absolute)).toBe(
      'https://app.temps.kfs.es/cli-login/CODE',
    )
  })

  test('resolves a path starting with / against the base URL', () => {
    expect(absoluteVerificationUri('https://app.temps.kfs.es', start)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })

  test('adds the missing leading slash for a bare relative path', () => {
    const relative = { ...start, verification_uri_complete: 'cli-login/ABCD-1234' }
    expect(absoluteVerificationUri('https://app.temps.kfs.es', relative)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })

  test('strips a trailing slash on the base URL before joining', () => {
    expect(absoluteVerificationUri('https://app.temps.kfs.es/', start)).toBe(
      'https://app.temps.kfs.es/cli-login/ABCD-1234',
    )
  })
})
