// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { connectLogStream } from './logStreamConnection'
import {
  buildLogStreamUrl,
  createReplayGate,
  mergeLogLines,
  parseLogFrame,
} from './useLogStream'

describe('buildLogStreamUrl', () => {
  test('requests a bounded Docker backlog for restart-heavy containers', () => {
    const url = new URL(
      buildLogStreamUrl(
        'ws://localhost/api/projects/1/environments/2/containers/abc/logs',
        'http://localhost',
        true,
        1000
      )
    )

    expect(url.searchParams.get('tail')).toBe('1000')
    expect(url.searchParams.get('timestamps')).toBe('true')
  })

  test('preserves existing filters while setting the effective tail', () => {
    const url = new URL(
      buildLogStreamUrl(
        '/api/logs?start_date=123&tail=all',
        'http://localhost:3013',
        false,
        500
      )
    )

    expect(url.searchParams.get('start_date')).toBe('123')
    expect(url.searchParams.get('tail')).toBe('500')
    expect(url.searchParams.get('timestamps')).toBe('false')
  })
})

// A log view wired the way `useLogStream` wires it: every socket asks for
// the same tail, each frame is parsed, the replay gate decides whether it
// replaces the buffer, and it is merged into a bounded buffer.
function logView(maxLogs: number) {
  const sockets: WebSocket[] = []
  let retry: (() => void) | undefined
  let buffer: string[] = []
  const gate = createReplayGate()
  const dispose = connectLogStream(
    () => {
      const ws = { close() {} } as WebSocket
      sockets.push(ws)
      return ws
    },
    () => {},
    (event, socketReplaced) => {
      if (socketReplaced) gate.socketReplaced()
      const frame = parseLogFrame(event.data as string)
      if (!frame) return
      buffer = mergeLogLines(
        buffer,
        [frame.text],
        maxLogs,
        gate.replacesBuffer(frame)
      )
    },
    ((fn: () => void) => {
      retry = fn
      return 1
    }) as unknown as typeof setTimeout,
    (() => {
      retry = undefined
    }) as typeof clearTimeout
  )
  const socket = () => sockets[sockets.length - 1]!
  return {
    lines: () => buffer,
    send(...lines: string[]) {
      const ws = socket()
      for (const line of lines) {
        ws.onmessage?.call(ws, { data: line } as MessageEvent)
      }
    },
    drop() {
      const ws = socket()
      ws.onclose?.call(ws, { code: 1006, reason: '' } as CloseEvent)
      const fn = retry
      retry = undefined
      fn?.()
    },
    dispose,
  }
}

describe('reconnecting the live log stream', () => {
  test('a reconnect replaying fewer than maxLogs lines shows each line once', () => {
    const view = logView(1000)
    view.send('booting', 'listening on :8080')
    view.drop()
    // The new socket asks for the same tail: Docker replays both lines,
    // then the stream goes on.
    view.send('booting', 'listening on :8080', 'GET /health 200')
    expect(view.lines()).toEqual([
      'booting',
      'listening on :8080',
      'GET /health 200',
    ])
    view.dispose()
  })

  test('a line logged while the socket was down is kept', () => {
    const view = logView(1000)
    view.send('one')
    view.drop()
    view.send('one', 'two')
    view.send('three')
    expect(view.lines()).toEqual(['one', 'two', 'three'])
    view.dispose()
  })

  test('only the first frame of a replacement socket replaces the buffer', () => {
    const view = logView(1000)
    view.send('a', 'b', 'c')
    expect(view.lines()).toEqual(['a', 'b', 'c'])
    view.dispose()
  })

  // What the server sends when Docker or the worker cannot be reached.
  const unavailable = JSON.stringify({
    error: 'Failed to get container logs',
    detail: 'worker node 2 is unreachable',
  })

  test('an error before the replay is appended, and the replay still replaces the lines', () => {
    const view = logView(1000)
    view.send('booting', 'listening on :8080')
    view.drop()
    view.send(unavailable)
    const shown = view.lines()
    expect(shown.slice(0, 2)).toEqual(['booting', 'listening on :8080'])
    expect(shown).toHaveLength(3)
    expect(shown[2]).toContain('worker node 2 is unreachable')
    // The replay that follows takes the buffer's place: no line twice.
    view.send('booting', 'listening on :8080', 'GET /health 200')
    expect(view.lines()).toEqual([
      'booting',
      'listening on :8080',
      'GET /health 200',
    ])
    view.dispose()
  })

  test('a reconnect that only sends an error keeps the lines on screen', () => {
    const view = logView(1000)
    view.send('booting', 'listening on :8080')
    view.drop()
    view.send(unavailable)
    expect(view.lines()).toHaveLength(3)
    expect(view.lines().slice(0, 2)).toEqual(['booting', 'listening on :8080'])
    // The next socket fails the same way: the lines are still there.
    view.drop()
    view.send(unavailable)
    expect(view.lines()).toHaveLength(4)
    expect(view.lines().slice(0, 2)).toEqual(['booting', 'listening on :8080'])
    // Once a socket replays, the replay replaces everything.
    view.drop()
    view.send('booting', 'listening on :8080')
    expect(view.lines()).toEqual(['booting', 'listening on :8080'])
    view.dispose()
  })
})

describe('parseLogFrame', () => {
  test("tells the server's error frames from log lines", () => {
    expect(
      parseLogFrame('{"error":"Failed to get container logs","detail":"x"}')
        ?.kind
    ).toBe('error')
    expect(parseLogFrame('{"error":"boom","stack":"at main"}')).toEqual({
      kind: 'error',
      text: 'ERROR: boom\nat main',
    })
    // A container's own JSON line that mentions an error is a log line.
    expect(
      parseLogFrame('{"level":"error","error":"timeout","msg":"retrying"}')
        ?.kind
    ).toBe('line')
    expect(parseLogFrame('{"message":"ready"}')).toEqual({
      kind: 'line',
      text: 'ready',
    })
    expect(parseLogFrame('plain text line ')).toEqual({
      kind: 'line',
      text: 'plain text line',
    })
    expect(parseLogFrame('   ')).toBeNull()
  })
})

describe('createReplayGate', () => {
  test('only the first log line after a replaced socket replaces the buffer', () => {
    const gate = createReplayGate()
    const line = { kind: 'line', text: 'a' } as const
    const error = { kind: 'error', text: 'e' } as const
    expect(gate.replacesBuffer(line)).toBe(false)
    gate.socketReplaced()
    expect(gate.replacesBuffer(error)).toBe(false)
    expect(gate.replacesBuffer(line)).toBe(true)
    expect(gate.replacesBuffer(line)).toBe(false)
    gate.socketReplaced()
    gate.reset()
    expect(gate.replacesBuffer(line)).toBe(false)
  })
})

describe('mergeLogLines', () => {
  test('appends within the bound and keeps the newest lines past it', () => {
    expect(mergeLogLines([1, 2], [3], 5, false)).toEqual([1, 2, 3])
    expect(mergeLogLines([1, 2, 3], [4, 5], 3, false)).toEqual([3, 4, 5])
  })

  test("a replay takes the buffer's place, still bounded", () => {
    expect(mergeLogLines([1, 2], [1, 2, 3], 5, true)).toEqual([1, 2, 3])
    expect(mergeLogLines([9], [1, 2, 3, 4], 2, true)).toEqual([3, 4])
    expect(mergeLogLines([1, 2], [], 5, true)).toEqual([])
  })
})
