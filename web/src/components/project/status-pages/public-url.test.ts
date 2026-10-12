// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { resolvePublicUrl } from './public-url'

describe('resolvePublicUrl', () => {
  test('resolves a console-relative link against the console origin', () => {
    expect(
      resolvePublicUrl('/api/status/abc123', 'https://console.example.test')
    ).toBe('https://console.example.test/api/status/abc123')
  })

  test('keeps the port of the origin the console is open on', () => {
    expect(
      resolvePublicUrl('/api/status/abc123', 'http://localhost:8080')
    ).toBe('http://localhost:8080/api/status/abc123')
  })

  test('leaves an absolute public URL untouched', () => {
    expect(
      resolvePublicUrl(
        'https://status.example.test/api/status/abc123',
        'https://console.example.test'
      )
    ).toBe('https://status.example.test/api/status/abc123')
  })
})
