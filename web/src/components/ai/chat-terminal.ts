// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The wire protocol of a sandbox terminal (`GET /v1/sandboxes/{id}/terminal`):
 * raw PTY bytes travel as binary frames in both directions, and control
 * messages as JSON text frames.
 */

/**
 * The tab the console attaches to. Reusing a tab id reattaches to the running
 * shell, so closing the page does not kill what is running in it, and it is
 * kept apart from any tab the assistant's own tooling may open.
 */
export const CONSOLE_TERMINAL_TAB = 'console'

/** Smallest size the server accepts is 1; below this a terminal is unusable. */
const MIN_DIMENSION = 2

export type TerminalEvent =
  | { type: 'ready'; tabId: string; existed: boolean }
  | { type: 'exit'; code: number }
  | { type: 'error'; message: string }

/** The WebSocket URL for a sandbox's terminal, on the page's own origin. */
export function terminalUrl(
  location: { protocol: string; host: string },
  sandboxId: string,
  size: { cols: number; rows: number },
  tab: string = CONSOLE_TERMINAL_TAB
): string {
  const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:'
  const query = new URLSearchParams({
    tab,
    cols: String(Math.max(MIN_DIMENSION, Math.floor(size.cols))),
    rows: String(Math.max(MIN_DIMENSION, Math.floor(size.rows))),
  })
  return `${scheme}//${location.host}/api/v1/sandboxes/${encodeURIComponent(
    sandboxId
  )}/terminal?${query.toString()}`
}

/** The control message that tells the shell the window changed size. */
export function resizeMessage(cols: number, rows: number): string {
  return JSON.stringify({
    type: 'resize',
    cols: Math.max(MIN_DIMENSION, Math.floor(cols)),
    rows: Math.max(MIN_DIMENSION, Math.floor(rows)),
  })
}

/** A server control frame, or `null` for anything that is not one. */
export function parseTerminalEvent(text: string): TerminalEvent | null {
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    return null
  }
  if (!value || typeof value !== 'object') return null
  const event = value as Record<string, unknown>
  switch (event.type) {
    case 'ready':
      return {
        type: 'ready',
        tabId: typeof event.tab_id === 'string' ? event.tab_id : '',
        existed: event.existed === true,
      }
    case 'exit':
      return {
        type: 'exit',
        code: typeof event.code === 'number' ? event.code : 0,
      }
    case 'error':
      return {
        type: 'error',
        message:
          typeof event.message === 'string'
            ? event.message
            : 'The terminal reported an error.',
      }
    default:
      return null
  }
}
