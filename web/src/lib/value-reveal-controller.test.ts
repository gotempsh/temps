// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { createValueReveal } from './value-reveal-controller'

function deferred() {
  let resolve!: (value: string) => void
  let reject!: (error: Error) => void
  const promise = new Promise<string>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

describe('value reveal controller', () => {
  test('reveals the fetched value, then hides it', async () => {
    const controller = createValueReveal(() =>
      Promise.resolve('https://x.test')
    )
    expect(controller.getState()).toEqual({ phase: 'hidden' })

    const pending = controller.reveal()
    expect(controller.getState()).toEqual({ phase: 'revealing' })
    await pending
    expect(controller.getState()).toEqual({
      phase: 'revealed',
      value: 'https://x.test',
    })

    controller.hide()
    expect(controller.getState()).toEqual({ phase: 'hidden' })
  })

  test('notifies subscribers on every transition', async () => {
    const controller = createValueReveal(() => Promise.resolve('v'))
    const seen: string[] = []
    controller.subscribe(() => seen.push(controller.getState().phase))
    await controller.reveal()
    controller.hide()
    expect(seen).toEqual(['revealing', 'revealed', 'hidden'])
  })

  test('a rejected request returns to hidden and keeps no value', async () => {
    const controller = createValueReveal(() => Promise.reject(new Error('403')))
    await controller.reveal()
    expect(controller.getState()).toEqual({ phase: 'hidden' })
  })

  test('a failed reveal can be retried', async () => {
    let calls = 0
    const controller = createValueReveal(() =>
      ++calls === 1 ? Promise.reject(new Error('boom')) : Promise.resolve('ok')
    )
    await controller.reveal()
    await controller.reveal()
    expect(controller.getState()).toEqual({ phase: 'revealed', value: 'ok' })
  })

  test('a second reveal while one is in flight does not refetch', async () => {
    const request = deferred()
    let calls = 0
    const controller = createValueReveal(() => {
      calls++
      return request.promise
    })
    const first = controller.reveal()
    await controller.reveal()
    request.resolve('v')
    await first
    expect(calls).toBe(1)
  })

  test('hiding while pending discards the late response', async () => {
    const request = deferred()
    const controller = createValueReveal(() => request.promise)
    const pending = controller.reveal()
    controller.hide()
    request.resolve('stale-secret-ish')
    await pending
    expect(controller.getState()).toEqual({ phase: 'hidden' })
  })

  test('disposing while pending (variable changed) discards the response', async () => {
    const request = deferred()
    const controller = createValueReveal(() => request.promise)
    const pending = controller.reveal()
    controller.dispose()
    request.resolve('old-value')
    await pending
    expect(controller.getState()).toEqual({ phase: 'hidden' })
  })

  test('a stale failure does not clobber a newer reveal', async () => {
    const first = deferred()
    const second = deferred()
    const queue = [first, second]
    const controller = createValueReveal(() => queue.shift()!.promise)
    const a = controller.reveal()
    controller.hide()
    const b = controller.reveal()
    second.resolve('fresh')
    await b
    first.reject(new Error('late'))
    await a
    expect(controller.getState()).toEqual({ phase: 'revealed', value: 'fresh' })
  })
})
