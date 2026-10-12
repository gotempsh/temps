// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import { submitLogin } from './login-submit'

/**
 * Lets any rejection nobody handled surface. `bun test` reports an unhandled
 * rejection as an error and fails the run, so reaching the assertions below
 * means none was raised.
 */
async function flushUnhandledRejections(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 20))
}

describe('submitLogin', () => {
  const credentials = { email: 'user@example.test', password: 'wrong' }

  test('a rejected sign-in resolves without an unhandled rejection', async () => {
    const loading: boolean[] = []
    const result = await submitLogin({
      credentials,
      login: () =>
        // The generated client rejects with the plain problem body.
        Promise.reject({ title: 'Unauthorized', status: 401 }),
      setLoading: (value) => loading.push(value),
    })
    await flushUnhandledRejections()
    expect(result).toBeUndefined()
    expect(loading).toEqual([true, false])
  })

  test('a successful sign-in passes the credentials through', async () => {
    const received: unknown[] = []
    const loading: boolean[] = []
    await submitLogin({
      credentials,
      login: (value) => {
        received.push(value)
        return Promise.resolve({ ok: true })
      },
      setLoading: (value) => loading.push(value),
    })
    expect(received).toEqual([credentials])
    expect(loading).toEqual([true, false])
  })
})
