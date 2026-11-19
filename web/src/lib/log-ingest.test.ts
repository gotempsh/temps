// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  curlSnippet,
  fluentBitSnippet,
  logIngestUrl,
  vectorSnippet,
} from './log-ingest'

const key = 'tlk_example'

describe('log ingest snippets', () => {
  test('the endpoint sits under the console origin', () => {
    expect(logIngestUrl('https://temps.example.com/')).toBe(
      'https://temps.example.com/api/logs/ingest'
    )
  })

  test('curl sends one NDJSON line with the key as a bearer token', () => {
    const snippet = curlSnippet(
      'https://temps.example.com/api/logs/ingest',
      key
    )
    expect(snippet).toContain(`Authorization: Bearer ${key}`)
    expect(snippet).toContain('application/x-ndjson')
  })

  test('Vector frames newline-delimited JSON', () => {
    const snippet = vectorSnippet(
      'https://temps.example.com/api/logs/ingest',
      key
    )
    expect(snippet).toContain('framing.method = "newline_delimited"')
    expect(snippet).toContain(`auth.token = "${key}"`)
  })

  test('Fluent Bit takes host, port and TLS from the URL', () => {
    const tls = fluentBitSnippet(
      'https://temps.example.com/api/logs/ingest',
      key
    )
    expect(tls).toContain('Host   temps.example.com')
    expect(tls).toContain('Port   443')
    expect(tls).toContain('tls    On')
    expect(tls).toContain('Format json_lines')
    const plain = fluentBitSnippet('http://10.0.0.5:8080/api/logs/ingest', key)
    expect(plain).toContain('Port   8080')
    expect(plain).toContain('tls    Off')
  })
})
