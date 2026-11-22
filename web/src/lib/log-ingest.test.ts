// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { CreatedLogIngestKeyResponse } from '@/api/client/types.gen'
import {
  createdKeyFor,
  curlSnippet,
  fluentBitSnippet,
  LOG_INGEST_PATH,
  logIngestUrl,
  vectorSnippet,
} from './log-ingest'

const key = 'tlk_example'

describe('log ingest snippets', () => {
  test('the endpoint sits under the public ingress URL', () => {
    expect(logIngestUrl('https://ingest.example.com/')).toBe(
      'https://ingest.example.com/api/logs/ingest'
    )
    expect(logIngestUrl('https://ingest.example.com')).toBe(
      `https://ingest.example.com${LOG_INGEST_PATH}`
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
    // Under the endpoint's 5 MiB and 10,000-line limits.
    const bytes = Number(/batch\.max_bytes = (\d+)/.exec(snippet)?.[1])
    const events = Number(/batch\.max_events = (\d+)/.exec(snippet)?.[1])
    expect(bytes).toBeGreaterThan(0)
    expect(bytes).toBeLessThan(5 * 1024 * 1024)
    expect(events).toBeGreaterThan(0)
    expect(events).toBeLessThanOrEqual(10_000)
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

const createdKey = {
  id: 3,
  name: 'host Vector',
  key: 'tlk_example_plaintext',
} as CreatedLogIngestKeyResponse

test('a created key is shown only on the service it was created for', () => {
  const created = { projectId: 4, key: createdKey }
  expect(createdKeyFor(created, 4)).toBe(createdKey)
  expect(createdKeyFor(created, 5)).toBeNull()
  expect(createdKeyFor(created, undefined)).toBeNull()
  expect(createdKeyFor(null, 4)).toBeNull()
})
