// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  installUnhandledRejectionReporter,
  markErrorReported,
  rejectionNotice,
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
