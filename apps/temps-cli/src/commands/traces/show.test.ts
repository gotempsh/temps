// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { Command } from 'commander'
import type { SpanRecord } from '../../api/types.gen.js'
import { registerShowTraceCommand, resolveTraceLookup, summarizeTrace } from './show.js'

const TRACE_ID = '4bf92f3577b34da6a3ce929d0e0e4736'

function span(overrides: Partial<SpanRecord>): SpanRecord {
  return {
    attributes: {},
    duration_ms: 10,
    end_time: '2026-11-25T14:02:00.010Z',
    events: [],
    kind: 'SERVER',
    name: 'GET /checkout',
    project_id: 4,
    resource: { service_name: 'storefront' },
    span_id: '00f067aa0ba902b7',
    start_time: '2026-11-25T14:02:00.000Z',
    status_code: 'OK',
    status_message: '',
    trace_id: TRACE_ID,
    ...overrides,
  } as SpanRecord
}

describe('resolveTraceLookup', () => {
  test('accepts a trace ID and a project, without a window', () => {
    expect(resolveTraceLookup(TRACE_ID, { projectId: '4' })).toEqual({
      projectId: 4,
      traceId: TRACE_ID,
    })
  })

  test('lower-cases the trace ID the way the store keeps it', () => {
    expect(resolveTraceLookup(TRACE_ID.toUpperCase(), { projectId: '4' }).traceId).toBe(TRACE_ID)
  })

  test('normalises a complete window to UTC ISO 8601', () => {
    expect(
      resolveTraceLookup(TRACE_ID, {
        projectId: '4',
        startTime: '2026-11-25T14:02:00Z',
        endTime: '2026-11-25T16:07:00+02:00',
      }).window,
    ).toEqual({
      start_time: '2026-11-25T14:02:00.000Z',
      end_time: '2026-11-25T14:07:00.000Z',
    })
  })

  test.each(['', 'abc', `${TRACE_ID}0`, 'zzf92f3577b34da6a3ce929d0e0e4736', `${TRACE_ID} --json`])(
    'rejects %p as a trace ID and names it',
    (value) => {
      expect(() => resolveTraceLookup(value, { projectId: '4' })).toThrow(
        `"${value}" is not a trace ID: expected 32 hexadecimal characters.`,
      )
    },
  )

  test.each([undefined, '', '0', '-1', 'storefront', '4; rm -rf /'])(
    'rejects %p as a project ID',
    (value) => {
      expect(() => resolveTraceLookup(TRACE_ID, { projectId: value })).toThrow(
        /--project-id must be the numeric ID/,
      )
    },
  )

  test('rejects half a window', () => {
    expect(() =>
      resolveTraceLookup(TRACE_ID, { projectId: '4', startTime: '2026-11-25T14:02:00Z' }),
    ).toThrow(/pass both or neither/)
    expect(() =>
      resolveTraceLookup(TRACE_ID, { projectId: '4', endTime: '2026-11-25T14:07:00Z' }),
    ).toThrow(/pass both or neither/)
  })

  test('rejects unparsable and inverted windows with the values given', () => {
    expect(() =>
      resolveTraceLookup(TRACE_ID, { projectId: '4', startTime: 'yesterday', endTime: 'now' }),
    ).toThrow('got "yesterday" and "now"')
    expect(() =>
      resolveTraceLookup(TRACE_ID, {
        projectId: '4',
        startTime: '2026-11-25T14:07:00Z',
        endTime: '2026-11-25T14:02:00Z',
      }),
    ).toThrow(/must be after --start-time/)
  })
})

describe('summarizeTrace', () => {
  test('an empty trace has nothing to summarise', () => {
    expect(summarizeTrace([])).toEqual({
      spans: 0,
      errorSpans: 0,
      services: [],
      root: null,
      startTime: null,
      durationMs: null,
    })
  })

  test('takes the root from the span with no parent, wherever it sits', () => {
    const summary = summarizeTrace([
      span({
        name: 'SELECT orders',
        span_id: 'b7ad6b7169203331',
        parent_span_id: '00f067aa0ba902b7',
        resource: { service_name: 'orders-db' } as SpanRecord['resource'],
        start_time: '2026-11-25T14:02:00.100Z',
        end_time: '2026-11-25T14:02:00.400Z',
        status_code: 'ERROR',
      }),
      span({ end_time: '2026-11-25T14:02:00.500Z' }),
    ])

    expect(summary).toEqual({
      spans: 2,
      errorSpans: 1,
      services: ['orders-db', 'storefront'],
      root: 'GET /checkout',
      startTime: '2026-11-25T14:02:00.000Z',
      durationMs: 500,
    })
  })

  test('falls back to the earliest span when the root was never recorded', () => {
    const summary = summarizeTrace([
      span({ name: 'late child', parent_span_id: 'aaaa', start_time: '2026-11-25T14:02:01.000Z' }),
      span({ name: 'early child', parent_span_id: 'aaaa' }),
    ])
    expect(summary.root).toBe('early child')
  })
})

describe('traces show registration', () => {
  test('exposes the flags a handed-over trace needs', () => {
    const traces = new Command('traces')
    registerShowTraceCommand(traces)
    const show = traces.commands.find((c) => c.name() === 'show')

    expect(show?.aliases()).toEqual(['get'])
    expect(show?.registeredArguments.map((a) => a.name())).toEqual(['trace-id'])
    expect(show?.options.map((o) => o.long)).toEqual([
      '--project-id',
      '--start-time',
      '--end-time',
      '--json',
    ])
    expect(show?.options.find((o) => o.long === '--project-id')?.mandatory).toBe(true)
  })
})
