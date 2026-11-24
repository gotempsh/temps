// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import { useEffect, useRef, useState } from 'react'
import { Terminal } from 'xterm'
import { FitAddon } from 'xterm-addon-fit'
import 'xterm/css/xterm.css'
import { parseTerminalEvent, resizeMessage, terminalUrl } from './chat-terminal'

type Connection =
  | { phase: 'connecting' }
  | { phase: 'open' }
  | { phase: 'closed'; reason: string }

/**
 * A shell in the chat's sandbox. The shell lives in the sandbox, not in this
 * page, so leaving and coming back reattaches to the same session.
 */
export function SandboxTerminal({ sandboxId }: { sandboxId: string }) {
  const hostRef = useRef<HTMLDivElement>(null)
  const [connection, setConnection] = useState<Connection>({
    phase: 'connecting',
  })
  const [attempt, setAttempt] = useState(0)

  useEffect(() => {
    const host = hostRef.current
    if (!host) return
    setConnection({ phase: 'connecting' })
    const terminal = new Terminal({
      cursorBlink: true,
      fontSize: 12,
      fontFamily:
        'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
      scrollback: 5000,
      allowProposedApi: false,
    })
    const fit = new FitAddon()
    terminal.loadAddon(fit)
    terminal.open(host)
    // A hidden tab has no size; fitting then would collapse the grid.
    const refit = () => {
      if (host.clientWidth === 0 || host.clientHeight === 0) return
      try {
        fit.fit()
      } catch {
        // The terminal was disposed between the resize and the fit.
      }
    }
    refit()

    const encoder = new TextEncoder()
    let disposed = false
    const socket = new WebSocket(
      terminalUrl(window.location, sandboxId, {
        cols: terminal.cols,
        rows: terminal.rows,
      })
    )
    socket.binaryType = 'arraybuffer'
    const close = (reason: string) => {
      if (disposed) return
      setConnection((current) =>
        current.phase === 'closed' ? current : { phase: 'closed', reason }
      )
    }
    socket.onopen = () => {
      if (!disposed) setConnection({ phase: 'open' })
    }
    socket.onmessage = (message) => {
      if (typeof message.data === 'string') {
        const event = parseTerminalEvent(message.data)
        if (event?.type === 'exit') {
          close(`The shell exited with code ${event.code}.`)
        } else if (event?.type === 'error') {
          close(event.message)
        }
        return
      }
      terminal.write(new Uint8Array(message.data as ArrayBuffer))
    }
    socket.onerror = () => {
      close(
        'Could not open a terminal. It needs permission to run commands in sandboxes, and the sandbox must be running.'
      )
    }
    socket.onclose = () => close('The terminal connection closed.')

    const input = terminal.onData((data) => {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(encoder.encode(data))
      }
    })
    const resized = terminal.onResize(({ cols, rows }) => {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(resizeMessage(cols, rows))
      }
    })
    const observer = new ResizeObserver(refit)
    observer.observe(host)

    return () => {
      disposed = true
      observer.disconnect()
      input.dispose()
      resized.dispose()
      socket.close()
      terminal.dispose()
    }
  }, [sandboxId, attempt])

  return (
    <div className="relative flex h-full min-h-[280px] flex-col overflow-hidden bg-black [&_.xterm-viewport]:[scrollbar-color:#3f3f46_#000]">
      {/* The fit addon sizes the grid from the host's computed height, which
          includes any padding on it, so the inset lives on a wrapper. Were the
          grid to overflow, the panel would gain a scrollbar, the narrower
          panel would refit, and the two would flicker back and forth. */}
      <div className="relative min-h-0 flex-1">
        <div ref={hostRef} className="absolute inset-2 overflow-hidden" />
      </div>
      {connection.phase === 'closed' && (
        <div
          role="alert"
          className="absolute inset-x-0 bottom-0 flex flex-wrap items-center justify-between gap-2 border-t bg-background px-3 py-2"
        >
          <p className="text-xs text-muted-foreground">{connection.reason}</p>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => setAttempt((count) => count + 1)}
          >
            Reconnect
          </Button>
        </div>
      )}
    </div>
  )
}
