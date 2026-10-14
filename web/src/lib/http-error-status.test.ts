// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { attachHttpStatus, httpStatusOf } from './http-error-status'
import { shouldRetryQuery } from './query-retry'
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

  test('gives a bodiless or non-JSON 4xx a status so it is not retried', () => {
    const empty = attachHttpStatus('', failed(404))
    expect(httpStatusOf(empty)).toBe(404)
    expect(isVerifiedNotFound(empty)).toBe(true)
    expect(shouldRetryQuery(0, empty)).toBe(false)

    // The generated client replaces a falsy error with `{}` afterwards, so
    // `undefined` and `null` bodies are covered as well.
    expect(isVerifiedNotFound(attachHttpStatus(undefined, failed(404)))).toBe(
      true
    )

    const html = attachHttpStatus(
      '<html><body>403 Forbidden</body></html>',
      failed(403)
    )
    expect(readFailureKind(html)).toBe('forbidden')
    expect(shouldRetryQuery(0, html)).toBe(false)
  })

  test('shows short plain text as the reason and never markup', () => {
    expect(attachHttpStatus('Not Found', failed(404))).toEqual({
      status: 404,
      title: 'Not Found',
      detail: 'Not Found',
    })
    expect(attachHttpStatus('<html>nginx</html>', failed(404))).toEqual({
      status: 404,
      title: 'Not Found',
    })
    expect(attachHttpStatus('x'.repeat(301), failed(404))).not.toHaveProperty(
      'detail'
    )
  })

  test('titles do not depend on the HTTP version', () => {
    // HTTP/2 responses have no statusText; the title comes from the status.
    expect(attachHttpStatus('', failed(401))).toMatchObject({
      title: 'Unauthorized',
    })
    expect(attachHttpStatus('', failed(418))).toMatchObject({
      title: 'HTTP 418',
    })
  })

  test('leaves a 5xx body as thrown so it keeps its retries', () => {
    expect(attachHttpStatus('<html>Bad Gateway</html>', failed(502))).toBe(
      '<html>Bad Gateway</html>'
    )
    expect(attachHttpStatus('', failed(502))).toBe('')
    expect(shouldRetryQuery(0, attachHttpStatus('', failed(502)))).toBe(true)
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
