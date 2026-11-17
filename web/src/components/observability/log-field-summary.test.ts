// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { logFieldSummary } from './log-field-summary'

test('skips fields the message already says', () => {
  expect(
    logFieldSummary(
      {
        method: 'GET',
        path: '/cart',
        status: 200,
        duration_ms: 34,
        request_id: 'req_1a2b',
      },
      'GET /cart 200 34ms'
    )
  ).toEqual([{ key: 'request_id', value: 'req_1a2b' }])
})

test('keeps at most three scalar fields, in the order the app wrote them', () => {
  expect(
    logFieldSummary(
      {
        order_id: 'ord_1',
        amount_cents: 1500,
        nested: { a: 1 },
        currency: 'EUR',
        extra: true,
      },
      'payment captured'
    )
  ).toEqual([
    { key: 'order_id', value: 'ord_1' },
    { key: 'amount_cents', value: '1500' },
    { key: 'currency', value: 'EUR' },
  ])
})

test('shortens long values from the middle', () => {
  expect(
    logFieldSummary(
      { trace_id: '18469588cff1dc2dee1d5819a2a74c26' },
      'latency above SLO'
    )
  ).toEqual([{ key: 'trace_id', value: '18469588cff…819a2a74c26' }])
})

test('plain-text lines have no fields to show', () => {
  expect(logFieldSummary(null, 'GET / HTTP/1.1')).toEqual([])
  expect(logFieldSummary(['a'], 'x')).toEqual([])
})
