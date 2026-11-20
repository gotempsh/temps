// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { MonitorResponse } from '../../api/types.gen.js'
import {
  checkSpecFromOptions,
  locationLines,
  MAX_HEADER_FILE_BYTES,
  MAX_REQUEST_BODY_BYTES,
  parseHeaderLines,
  readBounded,
  requestFromFiles,
} from './targets.js'

describe('checkSpecFromOptions', () => {
  it('builds each kind', () => {
    expect(checkSpecFromOptions('http', { method: 'post', expectStatus: '201, 2xx', keyword: 'ok', degradedAfterMs: '800' })).toEqual({
      kind: 'http',
      method: 'POST',
      expected_status: ['201', '2xx'],
      keyword: { text: 'ok', present: true },
      degraded_after_ms: 800,
    })
    expect(checkSpecFromOptions('tcp', {})).toEqual({ kind: 'tcp', degraded_after_ms: null })
    expect(checkSpecFromOptions('tls', { warnDays: '30' })).toEqual({ kind: 'tls', warn_days: 30 })
    expect(checkSpecFromOptions('dns', { record: 'cname', expect: 'edge.example.net' })).toEqual({
      kind: 'dns',
      record_type: 'CNAME',
      expected: ['edge.example.net'],
    })
  })

  it('refuses flags that do not apply', () => {
    const problem = (kind: string, options: Parameters<typeof checkSpecFromOptions>[1]) =>
      checkSpecFromOptions(kind, options) as string
    expect(problem('ping', {})).toContain('--kind must be one of')
    expect(problem('tcp', { keyword: 'x' })).toContain('only apply to --kind http')
    expect(problem('http', { record: 'A' })).toContain('only apply to --kind dns')
    expect(problem('http', { warnDays: '3' })).toContain('only applies to --kind tls')
    expect(problem('http', { method: 'FETCH' })).toContain('--method must be one of')
    expect(problem('http', { keywordAbsent: true })).toContain('needs --keyword')
    expect(problem('tls', { degradedAfterMs: '100' })).toContain('http and tcp')
    expect(problem('tcp', { degradedAfterMs: '-1' })).toContain('whole number')
    expect(problem('dns', { record: 'SRV' })).toContain('--record must be one of')
  })
})

describe('parseHeaderLines', () => {
  it('reads Name: value lines', () => {
    expect(parseHeaderLines('# comment\nAuthorization: Bearer a:b\n\nX-Env: prod\n')).toEqual([
      { name: 'Authorization', value: 'Bearer a:b' },
      { name: 'X-Env', value: 'prod' },
    ])
    expect(parseHeaderLines('no colon here')).toContain('line 1')
  })
})

describe('requestFromFiles', () => {
  it('reads the header and body files', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'temps-monitor-request-'))
    try {
      writeFileSync(join(dir, 'headers'), 'Authorization: Bearer example\n')
      writeFileSync(join(dir, 'body'), '{"ping":true}')
      expect(await requestFromFiles(join(dir, 'headers'), join(dir, 'body'))).toEqual({
        headers: [{ name: 'Authorization', value: 'Bearer example' }],
        body: '{"ping":true}',
      })
      expect(await requestFromFiles()).toBeNull()
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  it('refuses a file over the server limit without reading it whole', async () => {
    const dir = mkdtempSync(join(tmpdir(), 'temps-monitor-request-'))
    try {
      const body = join(dir, 'body')
      writeFileSync(body, 'x'.repeat(MAX_REQUEST_BODY_BYTES + 1))
      await expect(requestFromFiles(undefined, body)).rejects.toThrow(
        `The body file ${body} is larger than ${MAX_REQUEST_BODY_BYTES} bytes`,
      )
      const headers = join(dir, 'headers')
      writeFileSync(headers, `X-Pad: ${'x'.repeat(MAX_HEADER_FILE_BYTES)}\n`)
      await expect(requestFromFiles(headers)).rejects.toThrow('The header file')
      // Exactly at the limit is read.
      writeFileSync(body, 'x'.repeat(MAX_REQUEST_BODY_BYTES))
      expect((await requestFromFiles(undefined, body))?.body?.length).toBe(MAX_REQUEST_BODY_BYTES)
      // Multi-byte text is decoded whole across chunks.
      writeFileSync(body, 'é'.repeat(1000))
      expect(await readBounded(body, MAX_REQUEST_BODY_BYTES, 'The body file')).toBe('é'.repeat(1000))
    } finally {
      rmSync(dir, { recursive: true, force: true })
    }
  })

  it('names the file it could not read', async () => {
    const missing = join(tmpdir(), 'temps-monitor-request-missing', 'headers')
    await expect(requestFromFiles(missing)).rejects.toThrow(`could not read ${missing}`)
  })
})

describe('locationLines', () => {
  const monitor = (overrides: Partial<MonitorResponse>): MonitorResponse => ({
    id: 3,
    project_id: 1,
    environment_id: null,
    name: 'API',
    monitor_type: 'http',
    check_path: null,
    monitor_url: 'https://api.example.com/',
    check_kind: 'http',
    check: null,
    probe_from_nodes: true,
    location_results: null,
    check_interval_seconds: 60,
    is_active: true,
    is_managed: false,
    next_check_at: null,
    created_at: '2026-11-20T10:00:00Z',
    updated_at: '2026-11-20T10:00:00Z',
    ...overrides,
  })

  it('lists each location of the latest check', () => {
    expect(locationLines(monitor({ probe_from_nodes: false }))).toEqual([])
    expect(locationLines(monitor({}))).toEqual(['not checked since its last change'])
    expect(
      locationLines(
        monitor({
          location_results: {
            checked_at: '2026-11-20T10:01:00Z',
            results: [
              { location: 'control plane', node_id: null, region: null, status: 'operational', response_time_ms: 40, message: null },
              { location: 'worker-7', node_id: 7, region: 'eu', status: 'major_outage', response_time_ms: null, message: 'could not connect' },
              { location: 'worker-9', node_id: 9, region: null, status: null, response_time_ms: null, message: 'no answer: could not reach its agent' },
            ],
          },
        }),
      ),
    ).toEqual([
      'control plane: up, 40 ms',
      'worker-7 (eu): down — could not connect',
      'worker-9: no answer — no answer: could not reach its agent',
    ])
  })
})
