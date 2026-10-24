// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import {
  TERMINAL_INPUT_FRAME_BYTES,
  TERMINAL_REFUSED_HINT,
  chunkTerminalInput,
  computeTerminalSize,
  openTerminalSession,
  parseTerminalEvent,
  resizeMessage,
  terminalSocketUrl,
  terminalUnavailableReason,
  type TerminalStatus,
} from './sandbox-terminal'

class FakeSocket {
  binaryType = 'blob'
  readyState = 0
  onopen: (() => void) | null = null
  onmessage: ((event: { data: unknown }) => void) | null = null
  onerror: (() => void) | null = null
  onclose: (() => void) | null = null
  sent: unknown[] = []
  closedWith: number | undefined
  send(data: unknown) {
    this.sent.push(data)
  }
  close(code?: number) {
    this.closedWith = code
    this.readyState = 3
  }
  open() {
    this.readyState = 1
    this.onopen?.()
  }
  text(value: unknown) {
    this.onmessage?.({ data: JSON.stringify(value) })
  }
}

function start() {
  const socket = new FakeSocket()
  const statuses: Array<[TerminalStatus, string | undefined]> = []
  const output: number[][] = []
  const session = openTerminalSession({
    url: 'ws://example.test/terminal',
    createSocket: () => socket,
    onOutput: (bytes) => output.push(Array.from(bytes)),
    onStatus: (status, detail) => statuses.push([status, detail]),
    connectTimeoutMs: 50,
  })
  return { socket, statuses, output, session }
}

describe('terminalSocketUrl', () => {
  test('uses wss on https and carries tab and size', () => {
    expect(
      terminalSocketUrl({ protocol: 'https:', host: 'temps.test' }, 'sbx_1', {
        cols: 120,
        rows: 40,
      })
    ).toBe(
      'wss://temps.test/api/v1/sandboxes/sbx_1/terminal?tab=main&cols=120&rows=40'
    )
    expect(
      terminalSocketUrl({ protocol: 'http:', host: 'localhost:3000' }, 'a/b', {
        tab: 'build',
        cols: 80,
        rows: 24,
      })
    ).toBe(
      'ws://localhost:3000/api/v1/sandboxes/a%2Fb/terminal?tab=build&cols=80&rows=24'
    )
  })
})

describe('parseTerminalEvent', () => {
  test('parses the three server events', () => {
    expect(
      parseTerminalEvent('{"type":"ready","tab_id":"main","existed":true}')
    ).toEqual({ type: 'ready', tab_id: 'main', existed: true })
    expect(parseTerminalEvent('{"type":"exit","code":2}')).toEqual({
      type: 'exit',
      code: 2,
      signal: null,
    })
    expect(parseTerminalEvent('{"type":"error","message":"boom"}')).toEqual({
      type: 'error',
      message: 'boom',
    })
  })

  test('ignores anything else', () => {
    expect(parseTerminalEvent('not json')).toBeNull()
    expect(parseTerminalEvent('{"type":"mystery"}')).toBeNull()
    expect(parseTerminalEvent('null')).toBeNull()
  })
})

describe('sizes', () => {
  test('resizeMessage matches the wire shape', () => {
    expect(JSON.parse(resizeMessage(100, 30))).toEqual({
      type: 'resize',
      cols: 100,
      rows: 30,
    })
  })

  test('computeTerminalSize fits whole cells within bounds', () => {
    expect(computeTerminalSize(905, 410, 9, 17)).toEqual({
      cols: 100,
      rows: 24,
    })
    expect(computeTerminalSize(10, 10, 9, 17)).toEqual({ cols: 20, rows: 5 })
    expect(computeTerminalSize(90000, 90000, 9, 17)).toEqual({
      cols: 500,
      rows: 200,
    })
    expect(computeTerminalSize(500, 300, 0, 0)).toEqual({ cols: 80, rows: 24 })
  })
})

describe('terminalUnavailableReason', () => {
  const base = { status: 'running' }
  test('is null for a running sandbox, wherever it is placed', () => {
    expect(terminalUnavailableReason(base)).toBeNull()
  })
  test('explains stopped and destroyed sandboxes', () => {
    expect(terminalUnavailableReason({ ...base, status: 'stopped' })).toContain(
      'Resume'
    )
    expect(
      terminalUnavailableReason({ ...base, status: 'destroyed' })
    ).toContain('destroyed')
  })
  test('a sleeping workspace can still be opened (it wakes on use)', () => {
    expect(
      terminalUnavailableReason({
        ...base,
        status: 'stopped',
        lifecycle: 'workspace',
      })
    ).toBeNull()
  })
})

describe('openTerminalSession', () => {
  test('reports connecting, then connected on ready, and forwards output', () => {
    const { socket, statuses, output } = start()
    socket.open()
    socket.text({ type: 'ready', tab_id: 'main', existed: false })
    socket.onmessage?.({ data: new Uint8Array([104, 105]).buffer })
    expect(statuses.map(([status]) => status)).toEqual([
      'connecting',
      'connected',
    ])
    expect(socket.binaryType).toBe('arraybuffer')
    expect(output).toEqual([[104, 105]])
  })

  test('keystrokes are sent as binary and resize as JSON text', () => {
    const { socket, session } = start()
    socket.open()
    session.send('ls\r')
    session.resize(100, 30)
    expect(socket.sent[0]).toBeInstanceOf(Uint8Array)
    expect(Array.from(socket.sent[0] as Uint8Array)).toEqual([108, 115, 13])
    expect(JSON.parse(socket.sent[1] as string)).toEqual({
      type: 'resize',
      cols: 100,
      rows: 30,
    })
  })

  test('input is dropped while the socket is not open', () => {
    const { socket, session } = start()
    session.send('x')
    session.resize(80, 24)
    expect(socket.sent).toEqual([])
  })

  test('a close before ready is a refusal with the likely causes', () => {
    const { socket, statuses } = start()
    socket.onclose?.()
    expect(statuses[statuses.length - 1]).toEqual([
      'failed',
      TERMINAL_REFUSED_HINT,
    ])
  })

  test('a close after ready is reported as closed, not failed', () => {
    const { socket, statuses } = start()
    socket.open()
    socket.text({ type: 'ready', tab_id: 'main', existed: true })
    socket.onclose?.()
    expect(statuses[statuses.length - 1]?.[0]).toBe('closed')
  })

  test('an exit event wins over the close that follows it', () => {
    const { socket, statuses } = start()
    socket.open()
    socket.text({ type: 'ready', tab_id: 'main', existed: false })
    socket.text({ type: 'exit', code: 0, signal: null })
    socket.onclose?.()
    expect(statuses[statuses.length - 1]).toEqual([
      'exited',
      'The shell exited with code 0.',
    ])
  })

  test('a server error is surfaced with its message', () => {
    const { socket, statuses } = start()
    socket.open()
    socket.text({ type: 'error', message: 'spawn_failed' })
    expect(statuses[statuses.length - 1]).toEqual(['failed', 'spawn_failed'])
  })

  test('times out when the terminal never becomes ready', async () => {
    const { socket, statuses } = start()
    await new Promise((resolve) => setTimeout(resolve, 80))
    expect(statuses[statuses.length - 1]?.[0]).toBe('failed')
    expect(socket.closedWith).toBeUndefined()
    expect(socket.readyState).toBe(3)
  })

  test('closing by the user does not report a failure', () => {
    const { socket, statuses, session } = start()
    socket.open()
    socket.text({ type: 'ready', tab_id: 'main', existed: false })
    session.close()
    expect(socket.closedWith).toBe(1000)
    expect(statuses.map(([status]) => status)).toEqual([
      'connecting',
      'connected',
    ])
  })
})

describe('chunkTerminalInput', () => {
  test('keeps small input in one frame and drops empty input', () => {
    const small = new Uint8Array([1, 2, 3])
    expect(chunkTerminalInput(small)).toEqual([small])
    expect(chunkTerminalInput(new Uint8Array())).toEqual([])
  })

  test('splits a large paste in order, none over the frame size', () => {
    const bytes = new Uint8Array(TERMINAL_INPUT_FRAME_BYTES * 2 + 5).map(
      (_, index) => index % 251
    )
    const frames = chunkTerminalInput(bytes)
    expect(frames.length).toBe(3)
    expect(frames.every((f) => f.length <= TERMINAL_INPUT_FRAME_BYTES)).toBe(
      true
    )
    const joined = new Uint8Array(frames.reduce((n, f) => n + f.length, 0))
    let at = 0
    for (const frame of frames) {
      joined.set(frame, at)
      at += frame.length
    }
    expect(joined).toEqual(bytes)
  })

  test('sends a large paste as several binary frames', () => {
    const { socket, session } = start()
    socket.open()
    session.send('a'.repeat(TERMINAL_INPUT_FRAME_BYTES + 10))
    expect(socket.sent.length).toBe(2)
    expect((socket.sent[0] as Uint8Array).length).toBe(
      TERMINAL_INPUT_FRAME_BYTES
    )
    expect((socket.sent[1] as Uint8Array).length).toBe(10)
  })
})
