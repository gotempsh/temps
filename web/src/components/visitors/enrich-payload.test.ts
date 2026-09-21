// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { describe, expect, test } from 'bun:test'

import { buildEnrichPayload, isPayloadTooLargeError } from './enrich-payload'

describe('buildEnrichPayload', () => {
  test('sends removed keys as null so a merge actually deletes them', () => {
    expect(
      buildEnrichPayload(
        { email: 'a@example.com', plan: 'pro', name: 'Ada' },
        { email: 'b@example.com' }
      )
    ).toEqual({ email: 'b@example.com', plan: null, name: null })
  })

  test('keeps untouched keys out of the request only when they are edited away', () => {
    expect(buildEnrichPayload({ a: 1, b: 2 }, { a: 1, b: 2, c: 3 })).toEqual({
      a: 1,
      b: 2,
      c: 3,
    })
  })

  test('preserves an explicit null the operator typed themselves', () => {
    expect(buildEnrichPayload({ a: 1 }, { a: null })).toEqual({ a: null })
  })

  test('does not invent removals when there is no existing custom data', () => {
    for (const existing of [undefined, null, {}, [], 'nope', 7]) {
      expect(buildEnrichPayload(existing, { a: 1 })).toEqual({ a: 1 })
    }
  })

  test('clearing the whole document removes every existing key', () => {
    expect(buildEnrichPayload({ a: 1, b: 2 }, {})).toEqual({ a: null, b: null })
  })
})

describe('isPayloadTooLargeError', () => {
  test.each([
    'Failed to buffer the request body: length limit exceeded',
    { status: 413 },
    { statusCode: '413' },
    { title: 'Payload Too Large', detail: 'body exceeds limit' },
    { detail: 'Request entity too large' },
  ])('recognises the body-limit rejection %j', (error) => {
    expect(isPayloadTooLargeError(error)).toBe(true)
  })

  test.each([
    undefined,
    null,
    'Visitor not found',
    { status: 404 },
    { detail: 'Invalid visitor ID' },
    7,
  ])('does not mistake unrelated failures for the limit %j', (error) => {
    expect(isPayloadTooLargeError(error)).toBe(false)
  })
})
