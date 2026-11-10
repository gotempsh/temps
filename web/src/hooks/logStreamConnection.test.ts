// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { connectLogStream } from './logStreamConnection'

function harness() {
  const sockets: WebSocket[] = []
  const states: string[] = []
  const delays: number[] = []
  let pending: (() => void) | undefined
  const dispose = connectLogStream(
    () => {
      const ws = { close() {} } as WebSocket
      sockets.push(ws)
      return ws
    },
    (state) => states.push(state),
    () => {},
    ((fn: () => void, delay: number) => {
      pending = fn
      delays.push(delay)
      return 1
    }) as unknown as typeof setTimeout,
    (() => {
      pending = undefined
    }) as typeof clearTimeout
  )
  return {
    sockets,
    states,
    delays,
    dispose,
    tick() {
      const fn = pending
      pending = undefined
      fn?.()
    },
    close(code = 1006) {
      const ws = sockets[sockets.length - 1]!
      ws.onclose?.call(ws, { code, reason: '' } as CloseEvent)
    },
  }
}

test('first connection retries quietly at 500ms, 1s, 2s then reports failure', () => {
  const h = harness()
  for (let i = 0; i < 3; i++) {
    h.close()
    expect(h.states[h.states.length - 1]).toBe('connecting')
    h.tick()
  }
  h.close()
  expect(h.delays).toEqual([500, 1000, 2000])
  expect(h.sockets).toHaveLength(4)
  expect(h.states.filter((s) => s === 'error')).toHaveLength(1)
  h.dispose()
})

test('empty close retries; a completed stream with data does not', () => {
  const h = harness()
  h.close(1000)
  h.tick()
  const ws = h.sockets[h.sockets.length - 1]!
  ws.onmessage?.call(ws, { data: 'hello' } as MessageEvent)
  h.close(1000)
  expect(h.states[h.states.length - 1]).toBe('complete')
  expect(h.delays).toEqual([500])
  h.dispose()
})

test('cleanup cancels retries and ignores late events from replaced sockets', () => {
  const h = harness()
  const ws = h.sockets[0]
  const oldClose = ws.onclose!
  h.close()
  h.dispose()
  const count = h.states.length
  oldClose.call(ws, { code: 1006 } as CloseEvent)
  h.tick()
  expect(h.sockets).toHaveLength(1)
  expect(h.states).toHaveLength(count)
})

test('a working stream that drops gets a fresh retry budget', () => {
  const h = harness()
  h.close()
  h.tick()
  const ws = h.sockets[h.sockets.length - 1]!
  ws.onmessage?.call(ws, { data: 'hello' } as MessageEvent)
  h.close()
  h.tick()
  expect(h.delays).toEqual([500, 500])
  h.dispose()
})

test('an explicit server failure after an error payload does not retry forever', () => {
  const h = harness()
  const ws = h.sockets[0]
  ws.onmessage?.call(ws, { data: '{"error":"unavailable"}' } as MessageEvent)
  h.close(1011)
  expect(h.states[h.states.length - 1]).toBe('error')
  expect(h.delays).toEqual([])
  h.dispose()
})

test('the first frame of a replacement socket is marked as replacing the buffer', () => {
  const flags: boolean[] = []
  const sockets: WebSocket[] = []
  let retry: (() => void) | undefined
  const dispose = connectLogStream(
    () => {
      const ws = { close() {} } as WebSocket
      sockets.push(ws)
      return ws
    },
    () => {},
    (_event, replacesBuffer) => flags.push(replacesBuffer),
    ((fn: () => void) => {
      retry = fn
      return 1
    }) as unknown as typeof setTimeout,
    (() => {}) as typeof clearTimeout
  )
  const send = (data: string) => {
    const ws = sockets[sockets.length - 1]!
    ws.onmessage?.call(ws, { data } as MessageEvent)
  }
  send('a')
  send('b')
  const first = sockets[0]!
  first.onclose?.call(first, { code: 1006, reason: '' } as CloseEvent)
  retry?.()
  send('a')
  send('b')
  expect(flags).toEqual([false, false, true, false])
  dispose()
})
