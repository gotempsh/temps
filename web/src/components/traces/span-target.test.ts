// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { spanTarget } from './span-target'

test('a bare HTTP client span shows the host and path it called', () => {
  expect(
    spanTarget('POST', {
      'url.full': 'https://api.example.com/v1/payments?retry=1',
      'http.request.method': 'POST',
    })
  ).toBe('api.example.com/v1/payments')
  expect(
    spanTarget('GET', { 'server.address': 'search.internal', 'url.path': '/q' })
  ).toBe('search.internal/q')
})

test('a database span named after its operation shows the statement', () => {
  expect(
    spanTarget('GET', {
      'db.system': 'redis',
      'db.operation.name': 'GET',
      'db.query.text': 'GET session:{id}',
    })
  ).toBe('redis · session:{id}')
})

test('a span whose name already says what it did gets nothing', () => {
  expect(spanTarget('POST /checkout', { 'url.path': '/checkout' })).toBeNull()
  expect(
    spanTarget('payments.charge', { 'url.full': 'https://x.test/a' })
  ).toBeNull()
  expect(spanTarget('GET', {})).toBeNull()
})
