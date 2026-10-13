// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef, useState } from 'react'
import {
  Plug,
  PlugZap,
  RefreshCw,
  Terminal as TerminalIcon,
} from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { fitTerminalToHost } from './sandbox-terminal-dom'
import {
  openTerminalSession,
  terminalSocketUrl,
  type TerminalSession,
  type TerminalStatus,
} from './sandbox-terminal'

interface SandboxTerminalCardProps {
  sandboxId: string
  /** Why a terminal cannot be attached right now; `null` when it can. */
  unavailableReason: string | null
}

const STATUS_LABEL: Record<TerminalStatus, string> = {
  idle: 'Not connected',
  connecting: 'Connecting…',
  connected: 'Connected',
  exited: 'Exited',
  closed: 'Disconnected',
  failed: 'Could not connect',
}

/**
 * Interactive shell for a sandbox, over `GET /v1/sandboxes/{id}/terminal`.
 *
 * The terminal only connects when asked: every attached terminal holds a PTY
 * session and counts against the per-account limit. The shell keeps running
 * in the sandbox after disconnecting, and reconnecting resumes it.
 */
export function SandboxTerminalCard({
  sandboxId,
  unavailableReason,
}: SandboxTerminalCardProps) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const [active, setActive] = useState(false)
  const [attempt, setAttempt] = useState(0)
  const [status, setStatus] = useState<TerminalStatus>('idle')
  const [detail, setDetail] = useState<string | undefined>(undefined)

  useEffect(() => {
    const host = hostRef.current
    if (!active || !host) return undefined
    let disposed = false
    let session: TerminalSession | undefined
    let observer: ResizeObserver | undefined
    let cleanupTerminal: (() => void) | undefined

    const start = async () => {
      const [{ Terminal }] = await Promise.all([
        import('xterm'),
        import('xterm/css/xterm.css'),
      ])
      if (disposed) return
      const term = new Terminal({
        cursorBlink: true,
        fontFamily:
          'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
        fontSize: 13,
        scrollback: 5000,
        screenReaderMode: true,
        theme: { background: '#0b0b0c', foreground: '#e5e5e5' },
      })
      term.open(host)
      const fit = () => {
        fitTerminalToHost(term, host)
      }
      fit()

      session = openTerminalSession({
        url: terminalSocketUrl(window.location, sandboxId, {
          cols: term.cols,
          rows: term.rows,
        }),
        onOutput: (bytes) => term.write(bytes),
        onStatus: (next, message) => {
          if (disposed) return
          setStatus(next)
          setDetail(message)
          if (next === 'connected') term.focus()
        },
      })
      const data = term.onData((input) => session?.send(input))
      const resized = term.onResize(({ cols, rows }) =>
        session?.resize(cols, rows)
      )
      observer = new ResizeObserver(fit)
      observer.observe(host)
      cleanupTerminal = () => {
        data.dispose()
        resized.dispose()
        term.dispose()
      }
    }

    void start().catch(() => {
      if (disposed) return
      setStatus('failed')
      setDetail('The terminal could not be loaded. Reload the page and retry.')
    })

    return () => {
      disposed = true
      observer?.disconnect()
      session?.close()
      cleanupTerminal?.()
    }
  }, [active, attempt, sandboxId])

  const open = () => {
    setStatus('connecting')
    setDetail(undefined)
    setActive(true)
  }
  const disconnect = () => {
    setActive(false)
    setStatus('idle')
    setDetail(undefined)
  }
  const reconnect = () => {
    setStatus('connecting')
    setDetail(undefined)
    setAttempt((value) => value + 1)
  }

  const blocked = unavailableReason
  const ended =
    status === 'failed' || status === 'exited' || status === 'closed'

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <CardTitle className="flex items-center gap-2 text-sm font-medium">
            <TerminalIcon className="h-4 w-4" />
            Terminal
          </CardTitle>
          <div className="flex flex-wrap items-center gap-2">
            {active && (
              <Badge
                variant={
                  status === 'connected'
                    ? 'success-subtle'
                    : ended
                      ? 'warning-subtle'
                      : 'neutral-subtle'
                }
              >
                {STATUS_LABEL[status]}
              </Badge>
            )}
            {active && ended && (
              <Button size="sm" variant="outline" onClick={reconnect}>
                <RefreshCw className="mr-1.5 h-4 w-4" />
                Reconnect
              </Button>
            )}
            {active ? (
              <Button size="sm" variant="outline" onClick={disconnect}>
                <PlugZap className="mr-1.5 h-4 w-4" />
                Disconnect
              </Button>
            ) : (
              <Button size="sm" onClick={open} disabled={blocked !== null}>
                <Plug className="mr-1.5 h-4 w-4" />
                Open terminal
              </Button>
            )}
          </div>
        </div>
        {!active && (
          <CardDescription>
            A full interactive shell in this sandbox. Closing the terminal does
            not stop what is running; reconnecting resumes the same session.
          </CardDescription>
        )}
      </CardHeader>
      <CardContent className="space-y-3">
        {!active && blocked && (
          <Alert>
            <AlertTitle>Terminal unavailable</AlertTitle>
            <AlertDescription>{blocked}</AlertDescription>
          </Alert>
        )}
        {active && ended && detail && (
          <Alert variant={status === 'failed' ? 'destructive' : 'default'}>
            <AlertDescription>{detail}</AlertDescription>
          </Alert>
        )}
        <div
          ref={hostRef}
          role="group"
          aria-label="Sandbox terminal"
          className={
            active
              ? 'h-[420px] w-full overflow-hidden rounded-md bg-[#0b0b0c] p-2'
              : 'hidden'
          }
        />
      </CardContent>
    </Card>
  )
}
