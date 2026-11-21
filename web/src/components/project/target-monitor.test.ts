// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import type { MonitorResponse } from '@/api/client'
import {
  createTargetMonitorRequest,
  locationSummary,
  newTargetMonitorValues,
  splitList,
  targetMonitorSchema,
  targetMonitorValues,
  targetProblem,
  targetSummary,
  updateTargetMonitorRequest,
  type TargetMonitorValues,
} from './target-monitor'

function values(
  overrides: Partial<TargetMonitorValues> = {}
): TargetMonitorValues {
  return {
    ...newTargetMonitorValues(),
    name: 'Payments API',
    target: 'https://api.example.test/health',
    ...overrides,
  }
}

function monitor(check: MonitorResponse['check']): MonitorResponse {
  return {
    id: 7,
    project_id: 1,
    environment_id: null,
    name: 'Payments API',
    monitor_type: 'http',
    check_path: null,
    monitor_url: check?.target ?? '',
    check_kind: check?.check.kind ?? 'environment',
    check,
    probe_from_nodes: false,
    location_results: null,
    check_interval_seconds: 60,
    is_active: true,
    is_managed: false,
    next_check_at: null,
    created_at: '2026-11-21T10:00:00Z',
    updated_at: '2026-11-21T10:00:00Z',
  }
}

describe('targetProblem', () => {
  it('fits the target to the kind', () => {
    expect(targetProblem('http', 'https://api.example.test/')).toBeNull()
    expect(targetProblem('http', 'api.example.test')).toContain('full URL')
    expect(targetProblem('http', 'https://u:p@api.example.test/')).toContain(
      'header'
    )
    expect(targetProblem('tcp', 'db.example.test')).toContain('port')
    expect(targetProblem('tcp', 'db.example.test:5432')).toBeNull()
    expect(targetProblem('tcp', '[2001:db8::1]:22')).toBeNull()
    expect(targetProblem('tls', 'example.test')).toBeNull()
    expect(targetProblem('tls', 'https://example.test')).toContain('not a URL')
    expect(targetProblem('dns', 'example.test')).toBeNull()
    expect(targetProblem('dns', 'example.test:53')).toContain('host name')
  })
})

describe('targetMonitorSchema', () => {
  it('checks status patterns and HEAD keywords', () => {
    const issues = (input: TargetMonitorValues) => {
      const result = targetMonitorSchema.safeParse(input)
      return result.success
        ? []
        : result.error.issues.map((i) => i.path.join('.'))
    }
    expect(issues(values({ expectedStatus: '200, 3xx' }))).toEqual([])
    expect(issues(values({ expectedStatus: '20' }))).toEqual(['expectedStatus'])
    expect(issues(values({ method: 'HEAD', keyword: 'ok' }))).toEqual([
      'keyword',
    ])
    expect(issues(values({ degradedAfterMs: 'fast' }))).toEqual([
      'degradedAfterMs',
    ])
    expect(
      issues(values({ headers: [{ name: 'bad header', value: 'x' }] }))
    ).toEqual(['headers.0.name'])
  })
})

describe('requests', () => {
  it('builds an HTTP check with its request', () => {
    const body = createTargetMonitorRequest(
      values({
        expectedStatus: '200,\n204',
        keyword: ' "ok":true ',
        degradedAfterMs: '1500',
        headers: [
          { name: ' Authorization ', value: 'Bearer x' },
          { name: '', value: 'ignored' },
        ],
      })
    )
    expect(body.check).toEqual({
      kind: 'http',
      method: 'GET',
      expected_status: ['200', '204'],
      keyword: { text: '"ok":true', present: true },
      degraded_after_ms: 1500,
    })
    expect(body.request).toEqual({
      headers: [{ name: 'Authorization', value: 'Bearer x' }],
      body: null,
    })
    expect(body.probe_from_nodes).toBe(false)
    expect(
      createTargetMonitorRequest(values({ probeFromNodes: true }))
        .probe_from_nodes
    ).toBe(true)
  })

  it('sends no request for other kinds', () => {
    const body = createTargetMonitorRequest(
      values({
        kind: 'dns',
        target: 'example.test',
        recordType: 'CNAME',
        expectedValues: 'edge.example.net',
      })
    )
    expect(body.check).toEqual({
      kind: 'dns',
      record_type: 'CNAME',
      expected: ['edge.example.net'],
    })
    expect(body.request).toBeNull()
  })

  it('keeps stored headers unless the user replaces them', () => {
    const edited = targetMonitorValues(
      monitor({
        target: 'https://api.example.test/',
        check: { kind: 'http', method: 'POST', expected_status: ['201'] },
        header_names: ['Authorization'],
        has_body: true,
      })
    )
    expect(edited.method).toBe('POST')
    expect(edited.probeFromNodes).toBe(false)
    expect(updateTargetMonitorRequest(edited).probe_from_nodes).toBe(false)
    expect(edited.headers).toEqual([{ name: 'Authorization', value: '' }])
    expect(updateTargetMonitorRequest(edited).request).toBeNull()
    expect(
      updateTargetMonitorRequest({ ...edited, replaceRequest: true }).request
    ).toEqual({ headers: [{ name: 'Authorization', value: '' }], body: null })
  })
})

describe('editing keeps the saved timeout', () => {
  it('sends back a timeout set through the API', () => {
    const edited = targetMonitorValues(
      monitor({
        target: 'db.example.test:5432',
        check: { kind: 'tcp', degraded_after_ms: 20000, timeout_ms: 30000 },
        header_names: [],
        has_body: false,
      })
    )
    expect(
      updateTargetMonitorRequest({ ...edited, name: 'renamed' }).check
    ).toEqual({ kind: 'tcp', degraded_after_ms: 20000, timeout_ms: 30000 })
  })

  it('leaves the timeout to the server default for a new monitor', () => {
    expect(newTargetMonitorValues('dns').timeoutMs).toBeNull()
  })
})

describe('targetSummary', () => {
  it('says what is checked', () => {
    expect(
      targetSummary(
        monitor({
          target: 'example.test:443',
          check: { kind: 'tls', warn_days: 14 },
          header_names: [],
          has_body: false,
        })
      )
    ).toBe('TLS certificate of example.test:443')
    expect(targetSummary(monitor(null))).toBeNull()
    expect(splitList(' a, ,b\nc ')).toEqual(['a', 'b', 'c'])
  })
})

describe('locationSummary', () => {
  const spread = (
    results: NonNullable<MonitorResponse['location_results']>['results'] | null
  ) => ({
    ...monitor({
      target: 'https://api.example.test/',
      check: { kind: 'http' },
      header_names: [],
      has_body: false,
    }),
    probe_from_nodes: true,
    location_results: results
      ? { checked_at: '2026-11-21T10:01:00Z', results }
      : null,
  })

  it('says which locations failed or did not answer', () => {
    expect(locationSummary(monitor(null))).toBeNull()
    expect(locationSummary(spread(null))).toBe(
      'Checked from worker nodes; not checked yet'
    )
    expect(
      locationSummary(
        spread([
          {
            location: 'control plane',
            node_id: null,
            region: null,
            status: 'operational',
            response_time_ms: 30,
            message: null,
          },
          {
            location: 'worker-7',
            node_id: 7,
            region: 'eu',
            status: 'major_outage',
            response_time_ms: null,
            message: 'refused',
          },
          {
            location: 'worker-9',
            node_id: 9,
            region: null,
            status: null,
            response_time_ms: null,
            message: 'no answer: timeout',
          },
        ])
      )
    ).toBe(
      '2 of 3 locations answered; failing from worker-7 (eu); 1 did not answer'
    )
  })
})
