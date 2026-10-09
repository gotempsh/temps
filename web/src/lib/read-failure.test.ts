// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  isVerifiedNotFound,
  readFailureExplanation,
  readFailureKind,
  readFailureServerDetail,
} from './read-failure'

describe('readFailureKind', () => {
  test('only a numeric 404 is a missing record', () => {
    expect(readFailureKind({ title: 'Not Found', status: 404 })).toBe(
      'not-found'
    )
    expect(isVerifiedNotFound({ title: 'Not Found', status: 404 })).toBe(true)
    // A title alone proves nothing: react-query and fetch never set one.
    expect(isVerifiedNotFound({ title: 'Not Found' })).toBe(false)
  })

  test('401 and 403 are a permission gap, not a missing record', () => {
    expect(readFailureKind({ title: 'Forbidden', status: 403 })).toBe(
      'forbidden'
    )
    expect(readFailureKind({ title: 'Unauthorized', status: 401 })).toBe(
      'forbidden'
    )
  })

  test('server and network failures say nothing about the record', () => {
    expect(readFailureKind({ title: 'Server error', status: 500 })).toBe(
      'failed'
    )
    expect(readFailureKind(new TypeError('Failed to fetch'))).toBe('failed')
    expect(readFailureKind(undefined)).toBe('failed')
  })
})

describe('readFailureExplanation', () => {
  test('a forbidden read names the permission, a failed read the server', () => {
    expect(
      readFailureExplanation({ title: 'Forbidden', status: 403 })
    ).toContain('permission to read')
    expect(readFailureExplanation({ title: 'Oops', status: 502 })).toContain(
      'Could not contact Temps'
    )
  })
})

describe('readFailureServerDetail', () => {
  test("surfaces the server's Problem Details reason", () => {
    expect(
      readFailureServerDetail({
        title: 'Forbidden',
        status: 403,
        detail: 'Requires users:read permission',
      })
    ).toBe('Requires users:read permission')
  })

  test('never surfaces a client-side error message', () => {
    // These are the console's internals, not the server's reason.
    expect(
      readFailureServerDetail(new Error('["apiKey","1"] data is undefined'))
    ).toBeUndefined()
    expect(
      readFailureServerDetail(new TypeError('Failed to fetch'))
    ).toBeUndefined()
    expect(
      readFailureServerDetail({
        title: 'Server error',
        status: 500,
        detail: ' ',
      })
    ).toBeUndefined()
  })

  test('a malformed detail is ignored instead of throwing', () => {
    for (const detail of [500, { reason: 'nested' }, ['a'], true]) {
      expect(() =>
        readFailureServerDetail({ title: 'Server error', status: 500, detail })
      ).not.toThrow()
      expect(
        readFailureServerDetail({ title: 'Server error', status: 500, detail })
      ).toBeUndefined()
    }
  })
})
