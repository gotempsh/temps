// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import { checkSpecFromOptions, parseHeaderLines } from './targets.js'

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
