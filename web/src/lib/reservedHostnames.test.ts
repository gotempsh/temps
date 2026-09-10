// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { consoleHostname, reservedHostnameReason } from './reservedHostnames'

const settings = {
  external_url: 'https://console.example.com:8443/',
  preview_domain: 'apps.example.com',
}

describe('consoleHostname', () => {
  test('extracts the host from a full URL', () => {
    expect(consoleHostname(settings)).toBe('console.example.com')
  })

  test('accepts a bare host and normalizes case', () => {
    expect(consoleHostname({ external_url: 'Console.Example.com' })).toBe(
      'console.example.com'
    )
  })

  test('returns null when unset', () => {
    expect(consoleHostname({ external_url: '' })).toBeNull()
    expect(consoleHostname(undefined)).toBeNull()
  })
})

describe('reservedHostnameReason', () => {
  // Issue #478: assigning the console domain to a project made the console
  // unreachable until the operator went back in over the public IP.
  test('flags the console hostname', () => {
    expect(reservedHostnameReason('console.example.com', settings)).toContain(
      'console'
    )
    expect(
      reservedHostnameReason('CONSOLE.example.com.', settings)
    ).not.toBeNull()
  })

  test('flags the preview domain apex', () => {
    expect(reservedHostnameReason('apps.example.com', settings)).toContain(
      'preview domain'
    )
  })

  test('allows ordinary project domains and preview subdomains', () => {
    expect(reservedHostnameReason('shop.example.com', settings)).toBeNull()
    expect(
      reservedHostnameReason('my-app.apps.example.com', settings)
    ).toBeNull()
  })

  test('ignores wildcards and empty input', () => {
    expect(reservedHostnameReason('*.example.com', settings)).toBeNull()
    expect(reservedHostnameReason('', settings)).toBeNull()
  })

  test('is inert when the platform is unconfigured', () => {
    expect(reservedHostnameReason('anything.example.com', undefined)).toBeNull()
  })
})
