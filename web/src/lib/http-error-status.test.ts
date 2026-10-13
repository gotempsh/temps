// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { attachHttpStatus, httpStatusOf } from './http-error-status'
import { isVerifiedNotFound, readFailureKind } from './read-failure'

const failed = (status: number) => ({ ok: false, status })

describe('attachHttpStatus', () => {
  test('records the status line on a problem body that has none', () => {
    // What the server actually sends: title and detail, no status member.
    const body = { title: 'Project Not Found', detail: 'No project "demo"' }
    const error = attachHttpStatus(body, failed(404))
    expect(httpStatusOf(error)).toBe(404)
    expect(isVerifiedNotFound(error)).toBe(true)
  })

  test('lets a 500 and a 403 be told apart from a missing record', () => {
    const serverError = attachHttpStatus(
      { title: 'Internal Server Error', detail: 'database unavailable' },
      failed(500)
    )
    const forbidden = attachHttpStatus(
      { title: 'Insufficient Permissions', detail: 'Requires settings:read' },
      failed(403)
    )
    expect(readFailureKind(serverError)).toBe('failed')
    expect(readFailureKind(forbidden)).toBe('forbidden')
  })

  test('keeps a status the server put in the body', () => {
    const body = { title: 'Conflict', status: 409 }
    expect(httpStatusOf(attachHttpStatus(body, failed(500)))).toBe(409)
  })

  test('leaves network errors and successful responses alone', () => {
    const network = new TypeError('Failed to fetch')
    expect(attachHttpStatus(network, undefined)).toBe(network)
    expect(httpStatusOf(network)).toBeUndefined()

    const parseError = new SyntaxError('Unexpected token')
    attachHttpStatus(parseError, { ok: true, status: 200 })
    expect(httpStatusOf(parseError)).toBeUndefined()
  })

  test('does not rewrap a plain-text body', () => {
    // A proxy's HTML error page is thrown as a string; callers read it as one.
    expect(attachHttpStatus('<html>Bad Gateway</html>', failed(502))).toBe(
      '<html>Bad Gateway</html>'
    )
    expect(attachHttpStatus('', failed(502))).toBe('')
  })

  test('ignores array bodies and frozen objects', () => {
    const list = [1, 2]
    expect(attachHttpStatus(list, failed(400))).toBe(list)
    const frozen = Object.freeze({ title: 'Gone', detail: 'x' })
    expect(() => attachHttpStatus(frozen, failed(410))).not.toThrow()
  })
})

describe('httpStatusOf', () => {
  test('only accepts integer statuses', () => {
    expect(httpStatusOf({ status: 404 })).toBe(404)
    expect(httpStatusOf({ status: '404' })).toBeUndefined()
    expect(httpStatusOf({ status: 'error' })).toBeUndefined()
    expect(httpStatusOf(null)).toBeUndefined()
    expect(httpStatusOf('Not Found')).toBeUndefined()
  })
})
