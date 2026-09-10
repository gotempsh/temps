// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps sandbox shell` — an interactive terminal in a sandbox, in your own
 * terminal.
 *
 * Connects to `GET /v1/sandboxes/{id}/terminal`, which bridges to the
 * in-sandbox PTY agent (ADR-008). The agent keeps the tab alive with zero
 * subscribers, so this is a *reattachable* session: disconnecting — Ctrl-D,
 * a dropped network, closing your laptop — leaves whatever is running in
 * there running. Reconnect to the same `--tab` and you land back in it,
 * with recent scrollback replayed.
 *
 * That is what makes `temps sandbox shell ID --cmd claude` practical: the
 * agent survives your connection, so a long-running AI session isn't tied
 * to your laptop staying awake.
 *
 * Caveat worth knowing: suspension is not disconnection. The PTY lives in
 * the container's tmpfs, so if the sandbox is stopped (idle sweep, restart)
 * every tab is gone. While you're attached the server heartbeats activity
 * to keep the idle sweep away, so this only bites if you disconnect and
 * leave it idle past the timeout.
 */
import WebSocket from 'ws'

import { requireAuth, credentials, config } from '../../config/store.js'
import { normalizeApiUrl } from '../../lib/api-client.js'
import { colors, info, newline, warning } from '../../ui/output.js'
import { sandboxUrl } from './index.js'

export interface ShellOptions {
  tab?: string
  cmd?: string
}

/**
 * Turn the HTTP API base into the WebSocket URL for a sandbox terminal.
 *
 * `https` → `wss` (and `http` → `ws`); anything else is left alone so a
 * caller who already passed a `ws://` URL isn't second-guessed.
 */
export function terminalWsUrl(
  baseUrl: string,
  id: string,
  params: Record<string, string | undefined>,
): string {
  const http = sandboxUrl(baseUrl, `/${encodeURIComponent(id)}/terminal`)
  const ws = http.replace(/^http:/, 'ws:').replace(/^https:/, 'wss:')
  const query = Object.entries(params)
    .filter(([, v]) => v !== undefined && v !== '')
    .map(([k, v]) => `${k}=${encodeURIComponent(v as string)}`)
    .join('&')
  return query ? `${ws}?${query}` : ws
}

/** Current terminal size, falling back to a sane default when not a TTY. */
function terminalSize(): { cols: number; rows: number } {
  return {
    cols: process.stdout.columns || 80,
    rows: process.stdout.rows || 24,
  }
}

export async function shellAction(
  id: string,
  options: ShellOptions,
): Promise<void> {
  await requireAuth()
  const apiKey = await credentials.getApiKey()
  if (!apiKey) {
    throw new Error('Not authenticated. Run `temps login` first.')
  }
  if (!process.stdin.isTTY) {
    throw new Error(
      'sandbox shell needs an interactive terminal. ' +
        'For scripted commands use: temps sandbox exec ' +
        id +
        ' -- <command>',
    )
  }

  const baseUrl = normalizeApiUrl(config.get('apiUrl'))
  const { cols, rows } = terminalSize()
  const url = terminalWsUrl(baseUrl, id, {
    tab: options.tab,
    cmd: options.cmd,
    cols: String(cols),
    rows: String(rows),
  })

  // The token goes in a header, never the query string: URLs end up in
  // proxy logs and shell history, and this one is a live credential.
  const socket = new WebSocket(url, {
    headers: { Authorization: `Bearer ${apiKey}` },
  })

  await runSession(socket, id, options)
}

function runSession(
  socket: WebSocket,
  id: string,
  options: ShellOptions,
): Promise<void> {
  return new Promise((resolve, reject) => {
    let rawModeEngaged = false
    let exitCode = 0
    let settled = false
    // Did the *program* end, or did we just disconnect? The closing message
    // is a different fact in each case, and getting it wrong tells the user
    // their session survived when it did not.
    let programExited = false
    // Set when the user detached deliberately with the escape sequence, so
    // the closing message can confirm it rather than read like a dropout.
    let detachRequested = false
    // Whether this attach landed in an existing tab. Reported on exit so a
    // reattach is distinguishable from a fresh spawn without printing over
    // the replayed screen at connect time.
    let reattached = false

    // Every exit path goes through here. Leaving the terminal in raw mode
    // is the classic failure of this kind of tool — the user's shell is
    // then silently broken and needs `reset`.
    const restore = () => {
      if (rawModeEngaged && process.stdin.isTTY) {
        process.stdin.setRawMode(false)
        rawModeEngaged = false
      }
      process.stdin.pause()
      process.stdin.removeListener('data', onStdin)
      process.stdout.removeListener('resize', onResize)
    }

    const finish = (err?: Error) => {
      if (settled) return
      settled = true
      restore()
      if (err) reject(err)
      else resolve()
    }

    // Detach escape: Ctrl-P Ctrl-Q, the same sequence `docker attach` uses.
    //
    // Without it there is no way to leave a session alive from inside it —
    // `exit` ends the program (killing the tab), and every other key is
    // forwarded to the remote shell by design. Closing the window works but
    // is not something to have to reach for.
    const CTRL_P = 0x10
    const CTRL_Q = 0x11
    let sawCtrlP = false

    const onStdin = (chunk: Buffer) => {
      if (socket.readyState !== WebSocket.OPEN) return

      // Handle the escape without swallowing a legitimate Ctrl-P: only the
      // *pair* detaches. A lone Ctrl-P is held back one keystroke and then
      // forwarded, so readline's "previous history" still works — it just
      // arrives with the following key.
      for (let i = 0; i < chunk.length; i++) {
        const byte = chunk[i]!
        if (sawCtrlP) {
          sawCtrlP = false
          if (byte === CTRL_Q) {
            detachRequested = true
            socket.close()
            return
          }
          socket.send(Buffer.from([CTRL_P, byte]))
          continue
        }
        if (byte === CTRL_P) {
          sawCtrlP = true
          continue
        }
        socket.send(Buffer.from([byte]))
      }
    }

    const onResize = () => {
      if (socket.readyState !== WebSocket.OPEN) return
      const { cols, rows } = terminalSize()
      socket.send(JSON.stringify({ type: 'resize', cols, rows }))
    }

    socket.on('open', () => {
      process.stdin.setRawMode(true)
      rawModeEngaged = true
      process.stdin.resume()
      process.stdin.on('data', onStdin)
      process.stdout.on('resize', onResize)
    })

    socket.on('message', (data: Buffer, isBinary: boolean) => {
      if (isBinary) {
        process.stdout.write(data)
        return
      }
      // Text frames are control events, not terminal output — writing them
      // to stdout would splatter JSON into the user's session.
      let event: {
        type?: string
        code?: number
        message?: string
        existed?: boolean
      }
      try {
        event = JSON.parse(data.toString())
      } catch {
        return
      }
      if (event.type === 'ready') {
        reattached = event.existed === true
        // Say which one this is, once, before the replay arrives. Without it
        // there is no way to tell a restored session from a brand-new shell:
        // both just render a prompt, so a tab that legitimately ended looks
        // identical to reattach being broken.
        process.stdout.write(
          reattached
            ? `${colors.muted(`— reattached to tab '${options.tab ?? 'main'}', restoring recent output —`)}\r\n`
            : `${colors.muted(`— new session in tab '${options.tab ?? 'main'}' —`)}\r\n`,
        )
      } else if (event.type === 'exit') {
        exitCode = event.code ?? 0
        programExited = true
        socket.close()
      } else if (event.type === 'error') {
        restore()
        newline()
        warning(`Terminal error: ${event.message ?? 'unknown'}`)
      }
      // 'ready' needs no output: the shell's own prompt is the signal, and
      // announcing it would corrupt a reattached screen's replayed state.
    })

    socket.on('error', (err: Error) => {
      finish(
        new Error(
          `Could not open a terminal in ${id}: ${err.message}. ` +
            'Check the sandbox exists and is reachable with: temps sandbox show ' +
            id,
        ),
      )
    })

    socket.on('close', (code: number, reason: Buffer) => {
      restore()
      // 1000/1005/1006 are ordinary closes (clean, no-status, and the
      // "abnormal" code browsers use for a dropped TCP connection). Only a
      // genuine protocol-level failure is worth reporting as an error.
      if (code !== 1000 && code !== 1005 && code !== 1006) {
        const detail = reason.toString().trim()
        finish(
          new Error(
            `Terminal closed unexpectedly (code ${code})${detail ? `: ${detail}` : ''}`,
          ),
        )
        return
      }
      const tab = options.tab ?? 'main'
      newline()
      if (programExited) {
        // The program ended, so the agent dropped the tab with it. Saying
        // "anything running keeps running" here would be plainly false, and
        // it is what made a fresh shell on the next attach look like lost
        // history rather than the expected result of typing `exit`.
        info(
          `Session ended in ${colors.primary(id)} (tab ${tab}${
            exitCode !== 0 ? `, exit ${exitCode}` : ''
          }). ` +
            'That tab is gone — reattaching starts a fresh shell. To keep a ' +
            'session alive, detach with Ctrl-P Ctrl-Q or just close the ' +
            'window instead of exiting.',
        )
      } else {
        info(
          `${detachRequested ? 'Detached' : 'Disconnected'} from ${colors.primary(id)} (tab ${tab}). ` +
            'Anything running in it keeps running — reattach with the same ' +
            'command to pick up where you left off.',
        )
      }
      if (exitCode !== 0) {
        process.exitCode = exitCode
      }
      finish()
    })
  })
}
