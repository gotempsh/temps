// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  createDebouncedCommit,
  createUrlTextSync,
  type TimerApi,
} from './url-text-input'

describe('url text input sync', () => {
  test('echoes of our own writes never replace what the user has typed', () => {
    const sync = createUrlTextSync('')
    // Fast typing: "q", "qa", "qa-" are all written before the URL catches up.
    sync.committed('q')
    sync.committed('qa')
    sync.committed('qa-')
    expect(sync.observe('q')).toBe(false)
    expect(sync.observe('qa')).toBe(false)
    expect(sync.observe('qa-')).toBe(false)
  })

  test('a skipped intermediate URL value is still recognised as ours', () => {
    const sync = createUrlTextSync('')
    sync.committed('a')
    sync.committed('ab')
    expect(sync.observe('ab')).toBe(false)
  })

  test('adopts a value nobody here wrote (back button, followed link)', () => {
    const sync = createUrlTextSync('')
    sync.committed('abc')
    expect(sync.observe('abc')).toBe(false)
    expect(sync.observe('other')).toBe(true)
  })

  test('adopts an external clear', () => {
    const sync = createUrlTextSync('abc')
    expect(sync.observe('')).toBe(true)
  })

  test('re-observing the same URL value is a no-op', () => {
    const sync = createUrlTextSync('abc')
    expect(sync.observe('abc')).toBe(false)
  })

  test('a write that changes nothing leaves no stale echo behind', () => {
    const sync = createUrlTextSync('abc')
    sync.committed('abc')
    expect(sync.observe('xyz')).toBe(true)
    // `abc` arriving later is a navigation, not an echo of that write.
    expect(sync.observe('abc')).toBe(true)
  })

  test('a late echo after further typing does not rewind the box', () => {
    const sync = createUrlTextSync('')
    sync.committed('a')
    sync.committed('ab')
    sync.committed('abc')
    expect(sync.observe('ab')).toBe(false)
    expect(sync.observe('abc')).toBe(false)
    // The earlier "a" was dropped from the chain by the router; a later
    // genuine navigation to it must still be adopted.
    expect(sync.observe('a')).toBe(true)
  })
})

describe('url text input sync expiry', () => {
  // The router may land back on the original value without ever rendering
  // the intermediate one, so the write for it is never echoed.
  test('a write the router skipped stops counting as in flight', () => {
    let now = 0
    const sync = createUrlTextSync('a', { now: () => now })
    sync.committed('ab')
    sync.committed('a')
    expect(sync.observe('a')).toBe(false)
    now += 2_500
    // A followed link to `?search=ab` is a navigation, not our echo.
    expect(sync.observe('ab')).toBe(true)
  })

  test('a write is still recognised while it is within its age limit', () => {
    let now = 0
    const sync = createUrlTextSync('', { now: () => now })
    sync.committed('ab')
    now += 1_000
    expect(sync.observe('ab')).toBe(false)
  })
})

function fakeTimers() {
  let nextId = 1
  const queue = new Map<number, { at: number; callback: () => void }>()
  let clock = 0
  const api: TimerApi = {
    setTimeout: (callback, ms) => {
      const id = nextId++
      queue.set(id, { at: clock + ms, callback })
      return id
    },
    clearTimeout: (handle) => {
      queue.delete(handle as number)
    },
  }
  return {
    api,
    advance(ms: number) {
      clock += ms
      for (const [id, entry] of [...queue]) {
        if (entry.at <= clock) {
          queue.delete(id)
          entry.callback()
        }
      }
    },
    pending: () => queue.size,
  }
}

describe('debounced commit', () => {
  const setup = () => {
    const timers = fakeTimers()
    const written: string[] = []
    const commit = createDebouncedCommit(
      (text) => written.push(text),
      timers.api
    )
    return { timers, written, commit }
  }

  test('writes once, after the delay, with the latest text', () => {
    const { timers, written, commit } = setup()
    commit.set('q', { delayMs: 250 })
    timers.advance(100)
    commit.set('qa', { delayMs: 250 })
    timers.advance(100)
    commit.set('qa-', { delayMs: 250 })
    timers.advance(249)
    expect(written).toEqual([])
    timers.advance(1)
    expect(written).toEqual(['qa-'])
  })

  test('clearing the box writes immediately and drops the pending text', () => {
    const { timers, written, commit } = setup()
    commit.set('abc', { delayMs: 250 })
    commit.set('', { delayMs: 250 })
    expect(written).toEqual([''])
    timers.advance(1_000)
    expect(written).toEqual([''])
  })

  test('a zero delay and `immediate` write straight away', () => {
    const { written, commit } = setup()
    commit.set('a', { delayMs: 0 })
    commit.set('ab', { delayMs: 250, immediate: true })
    expect(written).toEqual(['a', 'ab'])
  })

  test('flush writes what is pending, once', () => {
    const { timers, written, commit } = setup()
    commit.set('abc', { delayMs: 250 })
    commit.flush()
    commit.flush()
    timers.advance(1_000)
    expect(written).toEqual(['abc'])
  })

  test('cancel (page unmount) leaves nothing to write', () => {
    const { timers, written, commit } = setup()
    commit.set('abc', { delayMs: 250 })
    commit.cancel()
    timers.advance(1_000)
    commit.flush()
    expect(written).toEqual([])
    expect(timers.pending()).toBe(0)
  })
})
