// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Client for `GET /v1/sandboxes/{id}/terminal`, the interactive PTY socket.
 *
 * Wire protocol (see `temps-sandbox/src/handlers/terminal.rs`):
 *  - binary frames carry raw PTY bytes in both directions;
 *  - text frames from the client are JSON control (`{"type":"resize",...}`);
 *  - text frames from the server are JSON events: `ready`, `exit`, `error`.
 *
 * The socket cannot report an HTTP status for a refused upgrade (permission,
 * stopped sandbox, terminal cap), so a connection that closes before `ready`
 * is described by its likely causes rather than by a status code.
 */

export type TerminalStatus =
  'idle' | 'connecting' | 'connected' | 'exited' | 'closed' | 'failed'

export type TerminalServerEvent =
  | { type: 'ready'; tab_id: string; existed: boolean }
  | { type: 'exit'; code: number | null; signal: number | null }
  | { type: 'error'; message: string }

/** How long to wait for the socket to open and report `ready`. */
export const TERMINAL_CONNECT_TIMEOUT_MS = 15_000

const MIN_COLS = 20
const MIN_ROWS = 5
const MAX_COLS = 500
const MAX_ROWS = 200

/** The tab the console attaches to; reusing it resumes the same shell. */
export const DEFAULT_TERMINAL_TAB = 'main'

export function terminalSocketUrl(
  location: Pick<Location, 'protocol' | 'host'>,
  sandboxId: string,
  options: { tab?: string; cols: number; rows: number }
): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
  const params = new URLSearchParams({
    tab: options.tab ?? DEFAULT_TERMINAL_TAB,
    cols: String(options.cols),
    rows: String(options.rows),
  })
  return `${protocol}//${location.host}/api/v1/sandboxes/${encodeURIComponent(sandboxId)}/terminal?${params.toString()}`
}

export function parseTerminalEvent(raw: string): TerminalServerEvent | null {
  try {
    const value: unknown = JSON.parse(raw)
    if (!value || typeof value !== 'object') return null
    const event = value as Record<string, unknown>
    if (event.type === 'ready' && typeof event.tab_id === 'string') {
      return {
        type: 'ready',
        tab_id: event.tab_id,
        existed: event.existed === true,
      }
    }
    if (event.type === 'exit') {
      return {
        type: 'exit',
        code: typeof event.code === 'number' ? event.code : null,
        signal: typeof event.signal === 'number' ? event.signal : null,
      }
    }
    if (event.type === 'error' && typeof event.message === 'string') {
      return { type: 'error', message: event.message }
    }
    return null
  } catch {
    return null
  }
}

export function resizeMessage(cols: number, rows: number): string {
  return JSON.stringify({ type: 'resize', cols, rows })
}

const clamp = (value: number, min: number, max: number) =>
  Math.min(max, Math.max(min, value))

/** Whole character cells that fit in a box, within sane bounds. */
export function computeTerminalSize(
  width: number,
  height: number,
  cellWidth: number,
  cellHeight: number
): { cols: number; rows: number } {
  if (!(cellWidth > 0) || !(cellHeight > 0)) {
    return { cols: 80, rows: 24 }
  }
  return {
    cols: clamp(Math.floor(width / cellWidth), MIN_COLS, MAX_COLS),
    rows: clamp(Math.floor(height / cellHeight), MIN_ROWS, MAX_ROWS),
  }
}

/** Why the terminal cannot be attached right now, or `null` when it can. */
export function terminalUnavailableReason(sandbox: {
  status: string
  node_id: number | null
  node_name: string
  lifecycle?: string
}): string | null {
  if (sandbox.status === 'destroyed') {
    return 'This sandbox has been destroyed.'
  }
  if (sandbox.node_id != null) {
    return `Terminals are not available yet for sandboxes on worker nodes. This sandbox runs on ${sandbox.node_name}; create one on the control plane to use a terminal.`
  }
  if (sandbox.status === 'stopped' && sandbox.lifecycle !== 'workspace') {
    return 'This sandbox is stopped. Resume it to open a terminal.'
  }
  return null
}

export const TERMINAL_REFUSED_HINT =
  'The terminal could not be opened. It needs the sandboxes:exec permission and a running sandbox, and each account can hold up to 4 terminals at once.'

interface SocketLike {
  binaryType: string
  readyState: number
  onopen: (() => void) | null
  onmessage: ((event: { data: unknown }) => void) | null
  onerror: (() => void) | null
  onclose: (() => void) | null
  send(data: string | ArrayBufferLike | ArrayBufferView): void
  close(code?: number): void
}

export interface TerminalSessionOptions {
  url: string
  onOutput: (bytes: Uint8Array) => void
  onStatus: (status: TerminalStatus, detail?: string) => void
  /** Injected for tests; defaults to the browser `WebSocket`. */
  createSocket?: (url: string) => SocketLike
  connectTimeoutMs?: number
}

export interface TerminalSession {
  /** Keystrokes from the terminal; sent as a binary PTY frame. */
  send: (data: string) => void
  resize: (cols: number, rows: number) => void
  close: () => void
}

const WS_OPEN = 1

/**
 * Largest input frame sent to the PTY. The endpoint closes the connection on
 * any message over 64 KiB, so a big paste is split rather than allowed to
 * kill the session. The PTY reassembles the byte stream, so a split inside a
 * multi-byte character is harmless.
 */
export const TERMINAL_INPUT_FRAME_BYTES = 16 * 1024

/** Split `bytes` into frames of at most `size` bytes, in order. */
export function chunkTerminalInput(
  bytes: Uint8Array,
  size: number = TERMINAL_INPUT_FRAME_BYTES
): Uint8Array[] {
  if (bytes.length <= size) return bytes.length === 0 ? [] : [bytes]
  const chunks: Uint8Array[] = []
  for (let offset = 0; offset < bytes.length; offset += size) {
    chunks.push(bytes.subarray(offset, offset + size))
  }
  return chunks
}

export function openTerminalSession(
  options: TerminalSessionOptions
): TerminalSession {
  const socket =
    options.createSocket?.(options.url) ??
    (new WebSocket(options.url) as unknown as SocketLike)
  socket.binaryType = 'arraybuffer'
  const encoder = new TextEncoder()
  let settled = false
  let ready = false

  const settle = (status: TerminalStatus, detail?: string) => {
    if (settled) return
    settled = true
    clearTimeout(timer)
    options.onStatus(status, detail)
  }

  options.onStatus('connecting')
  const timer = setTimeout(() => {
    if (ready) return
    settle(
      'failed',
      'Timed out waiting for the terminal. Check that the sandbox is running and reachable.'
    )
    socket.close()
  }, options.connectTimeoutMs ?? TERMINAL_CONNECT_TIMEOUT_MS)

  socket.onmessage = (event) => {
    const data = event.data
    if (typeof data === 'string') {
      const parsed = parseTerminalEvent(data)
      if (!parsed) return
      if (parsed.type === 'ready') {
        ready = true
        clearTimeout(timer)
        options.onStatus('connected', parsed.existed ? 'resumed' : 'new')
      } else if (parsed.type === 'exit') {
        settle(
          'exited',
          parsed.code != null
            ? `The shell exited with code ${parsed.code}.`
            : 'The shell exited.'
        )
      } else {
        settle('failed', parsed.message)
      }
      return
    }
    if (data instanceof ArrayBuffer) {
      options.onOutput(new Uint8Array(data))
    }
  }
  socket.onerror = () => {
    // The browser gives no reason for a refused upgrade; `onclose` follows.
  }
  socket.onclose = () => {
    clearTimeout(timer)
    if (settled) return
    if (ready) settle('closed', 'The connection was closed.')
    else settle('failed', TERMINAL_REFUSED_HINT)
  }

  return {
    send(data) {
      if (socket.readyState !== WS_OPEN) return
      for (const frame of chunkTerminalInput(encoder.encode(data))) {
        socket.send(frame)
      }
    },
    resize(cols, rows) {
      if (socket.readyState === WS_OPEN) {
        socket.send(resizeMessage(cols, rows))
      }
    },
    close() {
      settled = true
      clearTimeout(timer)
      socket.onmessage = null
      socket.onclose = null
      socket.onerror = null
      socket.close(1000)
    },
  }
}
