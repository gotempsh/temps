// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { test, expect, describe } from 'bun:test'
import { parseTarget } from './index.js'

describe('parseTarget', () => {
  test('defaults a bare host to port 80', () => {
    expect(parseTarget('example.com')).toEqual({ host: 'example.com', port: 80 })
  })

  test('keeps an explicit port on a bare host:port', () => {
    expect(parseTarget('localhost:8080')).toEqual({ host: 'localhost', port: 8080 })
  })

  test('defaults an https:// target with no port to 443', () => {
    expect(parseTarget('https://example.com')).toEqual({ host: 'example.com', port: 443 })
  })

  test('defaults an http:// target with no port to 80', () => {
    expect(parseTarget('http://example.com')).toEqual({ host: 'example.com', port: 80 })
  })

  test('keeps an explicit port on a target with a scheme', () => {
    expect(parseTarget('https://internal.svc:9443')).toEqual({
      host: 'internal.svc',
      port: 9443,
    })
  })
})
