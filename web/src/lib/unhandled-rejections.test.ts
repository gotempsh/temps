// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { MutationCache, QueryClient } from '@tanstack/react-query'
import {
  installUnhandledRejectionReporter,
  markErrorReported,
  rejectionNotice,
  wasErrorReported,
  type RejectionNotice,
} from './unhandled-rejections'

class FakeWindow {
  private listeners = new Set<(event: PromiseRejectionEvent) => void>()
  addEventListener(_type: string, listener: unknown) {
    this.listeners.add(listener as (event: PromiseRejectionEvent) => void)
  }
  removeEventListener(_type: string, listener: unknown) {
    this.listeners.delete(listener as (event: PromiseRejectionEvent) => void)
  }
  reject(reason: unknown) {
    let prevented = false
    const event = {
      reason,
      preventDefault: () => {
        prevented = true
      },
    } as unknown as PromiseRejectionEvent
    this.listeners.forEach((listener) => listener(event))
    return prevented
  }
  get listenerCount() {
    return this.listeners.size
  }
}

function install() {
  const fake = new FakeWindow()
  const notices: RejectionNotice[] = []
  const dispose = installUnhandledRejectionReporter(
    (notice) => notices.push(notice),
    fake as unknown as Window
  )
  return { fake, notices, dispose }
}

describe('rejectionNotice', () => {
  test('uses the problem title and detail of an API failure', () => {
    expect(
      rejectionNotice({
        title: 'Conflict',
        status: 409,
        detail: 'User admin@example.test already exists',
      })
    ).toEqual({
      title: 'Conflict',
      description: 'User admin@example.test already exists',
    })
  })

  test('has nothing to add for an error the user was already told about', () => {
    const problem = { title: 'Forbidden', status: 403, detail: 'No access' }
    markErrorReported(problem)
    expect(rejectionNotice(problem)).toBeUndefined()
  })

  test('has nothing to say about a value with no message', () => {
    expect(rejectionNotice({})).toBeUndefined()
    expect(rejectionNotice(undefined)).toBeUndefined()
  })
})

describe('installUnhandledRejectionReporter', () => {
  test('reports and swallows an unhandled API failure', () => {
    const { fake, notices } = install()
    const prevented = fake.reject({
      title: 'Internal Server Error',
      status: 500,
      detail: 'Database unavailable',
    })
    expect(prevented).toBe(true)
    expect(notices).toEqual([
      { title: 'Internal Server Error', description: 'Database unavailable' },
    ])
  })

  test('swallows but does not repeat a failure already toasted', () => {
    const { fake, notices } = install()
    const problem = { title: 'Bad Request', status: 400, detail: 'Nope' }
    markErrorReported(problem)
    expect(fake.reject(problem)).toBe(true)
    expect(notices).toEqual([])
  })

  test('keeps the browser report for an object it cannot explain', () => {
    const { fake, notices } = install()
    expect(fake.reject({ message: 'Save failed' })).toBe(false)
    expect(fake.reject({})).toBe(false)
    expect(notices).toEqual([])
  })

  test('leaves real errors to the browser', () => {
    const { fake, notices } = install()
    expect(fake.reject(new TypeError('x is undefined'))).toBe(false)
    expect(notices).toEqual([])
  })

  test('stops listening once disposed', () => {
    const { fake, dispose } = install()
    expect(fake.listenerCount).toBe(1)
    dispose()
    expect(fake.listenerCount).toBe(0)
  })
})

describe('mutations that handle their own failure', () => {
  test('the mutation cache marks the error even when the mutation has its own onError', async () => {
    const client = new QueryClient({
      mutationCache: new MutationCache({
        onError: (error) => markErrorReported(error),
      }),
    })
    const failure = { title: 'Conflict', status: 409, detail: 'Already there' }
    let announcedLocally = 0
    const result = await client
      .getMutationCache()
      .build(client, {
        mutationFn: async () => {
          throw failure
        },
        // Replaces the client's default onError, as most pages' own do.
        onError: () => {
          announcedLocally += 1
        },
      })
      .execute(undefined)
      .catch((error: unknown) => error)
    expect(result).toBe(failure)
    expect(announcedLocally).toBe(1)
    // The handler's `await mutateAsync` rejection is therefore not announced
    // a second time by the unhandled-rejection reporter.
    expect(wasErrorReported(failure)).toBe(true)
    expect(rejectionNotice(failure)).toBeUndefined()
  })
})
