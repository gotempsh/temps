// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  ClipboardPaste,
  Loader2,
  Power,
  RefreshCw,
  TextSelect,
  Terminal as TerminalIcon,
} from 'lucide-react'
import {
  useCallback,
  useEffect,
  useReducer,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { Link } from 'react-router'
import { toast } from 'sonner'
import type { Terminal } from 'xterm'
import type { ApplicationWorkspaceResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CopyButton } from '@/components/ui/copy-button'
import { Skeleton } from '@/components/ui/skeleton'
import { fitTerminalToHost } from '@/components/sandboxes/sandbox-terminal-dom'
import {
  openTerminalSession,
  TERMINAL_REFUSED_HINT,
  type TerminalSession,
} from '@/components/sandboxes/sandbox-terminal'
import {
  appendOutputTail,
  resyncReplay,
} from '@/components/sandboxes/terminal-replay'
import { cn } from '@/lib/utils'
import {
  classifyTerminalFailure,
  INITIAL_TERMINAL_VIEW_STATE,
  TERMINAL_HELPER_MISSING_HINT,
  terminalConnectTimeoutMs,
  terminalKeySequence,
  terminalViewBadge,
  terminalViewReducer,
  workspaceTerminalAvailability,
  workspaceTerminalSocketUrl,
  type BackendTerminalSupport,
  type TerminalKey,
  type TerminalViewState,
  type WorkspaceTerminalAvailability,
} from './workspace-terminal'

const TERMINAL_FONT =
  'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace'

const EXTRA_KEYS: Array<{
  key: TerminalKey
  label: string
  icon?: typeof ArrowUp
}> = [
  { key: 'escape', label: 'Esc' },
  { key: 'tab', label: 'Tab' },
  { key: 'ctrl-c', label: 'Ctrl-C' },
  { key: 'up', label: 'Up arrow', icon: ArrowUp },
  { key: 'down', label: 'Down arrow', icon: ArrowDown },
  { key: 'left', label: 'Left arrow', icon: ArrowLeft },
  { key: 'right', label: 'Right arrow', icon: ArrowRight },
]

export interface WorkspaceTerminalPanelProps {
  workspace: ApplicationWorkspaceResponse | null
  loading: boolean
  /** A wake is in flight (from the automatic wake or the Wake button). */
  waking: boolean
  /**
   * The application whose workspace this is, or `null` for the user's global
   * workspace. Application workspaces have a control action to wake them; the
   * global one is woken by the attach itself.
   */
  applicationPublicId: string | null
  /** The terminal thread this shell belongs to; the server derives its tab. */
  threadPublicId: string
  /** An archived thread is read-only until restored: no shell is attached. */
  archived?: boolean
  /** The signed-in user's role, to explain a missing `sandboxes:exec`. */
  role: string | null | undefined
  /** Per-backend terminal support, when the backend reports it. */
  backendTerminal?: BackendTerminalSupport | null
  onWake: () => void
  onOpenWorkspaceSettings: () => void
}

/**
 * Interactive shell inside the persistent AI workspace sandbox.
 *
 * Attaches over `GET /v1/sandboxes/{id}/terminal` (the sandbox provider's
 * `attach_pty`, so it is backend-agnostic) to the shell a terminal thread
 * owns, so leaving the thread, reloading or losing the connection detaches
 * without stopping what is running and the next attach picks it up again.
 */
export function WorkspaceTerminalPanel({
  workspace,
  loading,
  waking,
  applicationPublicId,
  threadPublicId,
  archived = false,
  role,
  backendTerminal,
  onWake,
  onOpenWorkspaceSettings,
}: WorkspaceTerminalPanelProps) {
  const availability = workspaceTerminalAvailability({
    workspace,
    loading,
    waking,
    hasApplication: applicationPublicId !== null,
    role,
    backendTerminal,
  })
  const hostRef = useRef<HTMLDivElement | null>(null)
  const [terminal, setTerminal] = useState<Terminal | null>(null)
  const [view, dispatch] = useReducer(
    terminalViewReducer,
    INITIAL_TERMINAL_VIEW_STATE
  )
  const sessionRef = useRef<TerminalSession | null>(null)
  // The end of the output the terminal has been shown, used to continue its
  // history across a reconnect.
  const outputTailRef = useRef<Uint8Array>(new Uint8Array(0))
  const [generation, setGeneration] = useState(0)
  const [connectRequested, setConnectRequested] = useState(false)
  // Waking by attaching is a container start, so it needs a longer wait than
  // attaching to a sandbox that is already up.
  const [connectTimeoutMs, setConnectTimeoutMs] = useState(
    terminalConnectTimeoutMs(false)
  )
  const [selection, setSelection] = useState('')

  // A suspended global workspace has no control action, and a workspace that
  // was never created has nothing to resume: the attach itself wakes or
  // creates the sandbox, so connecting only starts once the user asks.
  const wakesOnConnect =
    availability.kind === 'not-created' ||
    (availability.kind === 'sleeping' && availability.wake === 'on-connect')
  const canConnect =
    !archived &&
    (availability.kind === 'ready' || (wakesOnConnect && connectRequested))
  // Latch the thread once it may be attached. Later status polls (a stale
  // `sleeping` while a live terminal is open, a slow refresh) must not tear
  // down a working session. The sandbox's id cannot key the session: it may
  // not exist until the attach creates it, and keying on it would remount the
  // terminal the moment it appears. A terminal thread owns its shell, so the
  // thread does.
  const [target, setTarget] = useState<string | null>(null)
  if (canConnect && target !== threadPublicId) {
    setTarget(threadPublicId)
  }

  // The xterm instance lives as long as there is a sandbox to show, across
  // reconnects, so scrollback and the viewport survive a dropped socket.
  useEffect(() => {
    const host = hostRef.current
    if (!target || !host) return undefined
    let disposed = false
    let created: Terminal | undefined
    let observer: ResizeObserver | undefined
    let frame = 0
    let selectionListener: { dispose: () => void } | undefined

    const mount = async () => {
      const [{ Terminal: XTerm }] = await Promise.all([
        import('xterm'),
        import('xterm/css/xterm.css'),
      ])
      if (disposed) return
      const term = new XTerm({
        cursorBlink: true,
        fontFamily: TERMINAL_FONT,
        fontSize: 13,
        scrollback: 5000,
        screenReaderMode: true,
        theme: { background: '#0b0b0c', foreground: '#e5e5e5' },
      })
      created = term
      term.open(host)
      fitTerminalToHost(term, host)
      // One fit per frame: a drag-resize fires the observer continuously, and
      // every size change is a message to the shell.
      observer = new ResizeObserver(() => {
        if (frame) return
        frame = window.requestAnimationFrame(() => {
          frame = 0
          fitTerminalToHost(term, host)
        })
      })
      observer.observe(host)
      selectionListener = term.onSelectionChange(() =>
        setSelection(term.getSelection())
      )
      setTerminal(term)
    }

    void mount().catch(() => {
      if (disposed) return
      dispatch({
        type: 'status',
        status: 'failed',
        detail: 'The terminal could not be loaded. Reload the page and retry.',
        at: Date.now(),
      })
    })

    return () => {
      disposed = true
      if (frame) window.cancelAnimationFrame(frame)
      observer?.disconnect()
      selectionListener?.dispose()
      created?.dispose()
      outputTailRef.current = new Uint8Array(0)
      setTerminal(null)
      setSelection('')
    }
  }, [target])

  // One socket per attempt. The terminal outlives it.
  useEffect(() => {
    if (!terminal || !target) return undefined
    let closed = false
    // The server replays recent output as the first message of an attach. The
    // terminal keeps its own, longer history across reconnects, so the replay
    // is trimmed to what it has not shown yet rather than clearing it.
    let firstOutput = true
    const session = openTerminalSession({
      url: workspaceTerminalSocketUrl(window.location, applicationPublicId, {
        thread: threadPublicId,
        cols: terminal.cols,
        rows: terminal.rows,
      }),
      connectTimeoutMs,
      onOutput: (received) => {
        let bytes = received
        if (firstOutput) {
          firstOutput = false
          const resync = resyncReplay(outputTailRef.current, received)
          if (resync.reset) {
            terminal.reset()
            outputTailRef.current = new Uint8Array(0)
          }
          bytes = resync.bytes
        }
        outputTailRef.current = appendOutputTail(outputTailRef.current, bytes)
        terminal.write(bytes)
      },
      onStatus: (status, detail) => {
        if (closed) return
        dispatch({
          type: 'status',
          status,
          detail,
          at: Date.now(),
          roll: Math.random(),
        })
        if (status === 'connected') {
          // The window may have changed size while connecting.
          session.resize(terminal.cols, terminal.rows)
          terminal.focus()
        }
      },
    })
    sessionRef.current = session
    const data = terminal.onData((input) => session.send(input))
    const resized = terminal.onResize(({ cols, rows }) =>
      session.resize(cols, rows)
    )
    return () => {
      closed = true
      if (sessionRef.current === session) sessionRef.current = null
      data.dispose()
      resized.dispose()
      session.close()
    }
  }, [
    terminal,
    target,
    threadPublicId,
    generation,
    connectTimeoutMs,
    applicationPublicId,
  ])

  useEffect(() => {
    if (view.phase !== 'reconnecting') return undefined
    const timer = window.setTimeout(() => {
      dispatch({ type: 'reconnect-due' })
      setGeneration((value) => value + 1)
    }, view.delayMs)
    return () => window.clearTimeout(timer)
  }, [view])

  const retry = useCallback(() => {
    dispatch({ type: 'retry' })
    setGeneration((value) => value + 1)
  }, [])

  const sendKey = (key: TerminalKey) => {
    if (!terminal) return
    const applicationCursor = terminal.modes.applicationCursorKeysMode
    sessionRef.current?.send(terminalKeySequence(key, applicationCursor))
    terminal.focus()
  }

  const paste = async () => {
    if (!terminal) return
    try {
      const text = await navigator.clipboard.readText()
      if (text) terminal.paste(text)
      terminal.focus()
    } catch {
      toast.error("Couldn't read the clipboard", {
        description:
          'Your browser blocked clipboard access. Press and hold in the terminal, or use Ctrl+Shift+V, to paste.',
      })
    }
  }

  const showTerminal = target !== null
  const badge = terminalViewBadge(view)
  const lostConnection =
    view.phase === 'disconnected' ||
    view.phase === 'exited' ||
    view.phase === 'refused'

  return (
    <div className="flex h-full min-h-[360px] flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2 text-xs font-medium">
          <TerminalIcon className="size-4 shrink-0 stroke-success" />
          <span className="truncate">Shell in the workspace sandbox</span>
        </div>
        {showTerminal && (
          <Badge variant={badgeVariant(badge.tone)} role="status">
            {badge.label}
          </Badge>
        )}
      </div>

      {!showTerminal && archived && (
        <OnboardingCard
          detail="Restore this thread from the thread list to attach to its shell again. Whatever was running in it is still there."
          title="This terminal thread is archived"
        />
      )}

      {!showTerminal && !archived && (
        <WorkspaceTerminalOnboarding
          availability={availability}
          onOpenWorkspaceSettings={onOpenWorkspaceSettings}
          onWake={onWake}
          onWakeOnConnect={() => {
            setConnectTimeoutMs(terminalConnectTimeoutMs(true))
            setConnectRequested(true)
          }}
        />
      )}

      {showTerminal && (
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div
            aria-label="Extra keys"
            className="flex flex-wrap items-center gap-1 md:hidden"
            role="group"
          >
            {EXTRA_KEYS.map(({ key, label, icon: Icon }) => (
              <Button
                aria-label={label}
                className="h-8 min-w-8 px-2 text-xs"
                disabled={view.phase !== 'connected'}
                key={key}
                onClick={() => sendKey(key)}
                size="sm"
                title={label}
                type="button"
                variant="outline"
              >
                {Icon ? <Icon className="size-3.5" /> : label}
              </Button>
            ))}
          </div>
          <div className="ml-auto flex items-center gap-1">
            <Button
              aria-label="Select all terminal text"
              className="size-8"
              disabled={!terminal}
              onClick={() => terminal?.selectAll()}
              size="icon"
              title="Select all"
              type="button"
              variant="outline"
            >
              <TextSelect className="size-3.5" />
            </Button>
            <CopyButton
              className="size-8 rounded-md border border-input"
              disabled={!selection}
              label="Copy selected terminal text"
              value={selection}
            />
            <Button
              aria-label="Paste into terminal"
              className="size-8"
              disabled={view.phase !== 'connected'}
              onClick={() => void paste()}
              size="icon"
              title="Paste"
              type="button"
              variant="outline"
            >
              <ClipboardPaste className="size-3.5" />
            </Button>
          </div>
        </div>
      )}

      <div
        aria-label="Workspace terminal"
        className={cn(
          'min-h-[240px] w-full flex-1 overflow-hidden rounded-md bg-[#0b0b0c] p-2',
          !showTerminal && 'hidden'
        )}
        ref={hostRef}
        role="group"
      />

      {showTerminal && lostConnection && (
        <ConnectionLostNotice
          onOpenWorkspaceSettings={onOpenWorkspaceSettings}
          onRetry={retry}
          view={view}
        />
      )}
    </div>
  )
}

function badgeVariant(tone: ReturnType<typeof terminalViewBadge>['tone']) {
  switch (tone) {
    case 'success':
      return 'success-subtle' as const
    case 'warning':
      return 'warning-subtle' as const
    case 'danger':
      return 'destructive' as const
    case 'neutral':
      return 'neutral-subtle' as const
  }
}

function ConnectionLostNotice({
  view,
  onRetry,
  onOpenWorkspaceSettings,
}: {
  view: TerminalViewState
  onRetry: () => void
  onOpenWorkspaceSettings: () => void
}) {
  const reason =
    view.phase === 'disconnected' ||
    view.phase === 'exited' ||
    view.phase === 'refused'
      ? view.reason
      : ''
  const exited = view.phase === 'exited'
  const refused = view.phase === 'refused'
  const helperMissing =
    !exited && classifyTerminalFailure(reason) === 'helper-missing'
  return (
    <div
      className="space-y-2 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-xs"
      role="alert"
    >
      <p className="font-medium">
        {exited
          ? 'The shell exited'
          : refused
            ? 'The terminal could not be opened'
            : 'Disconnected from the workspace terminal'}
      </p>
      <p className="leading-5 text-muted-foreground">{reason}</p>
      {helperMissing && (
        <p className="leading-5 text-muted-foreground">
          {TERMINAL_HELPER_MISSING_HINT}
        </p>
      )}
      {refused && !helperMissing && (
        <p className="leading-5 text-muted-foreground">
          {TERMINAL_REFUSED_HINT}
        </p>
      )}
      {!exited && !refused && (
        <p className="leading-5 text-muted-foreground">
          Whatever was running keeps running in the sandbox. Reconnecting picks
          the same session back up.
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button onClick={onRetry} size="sm" type="button" variant="outline">
          <RefreshCw className="mr-1.5 size-3.5" />
          {exited ? 'Start a new session' : 'Reconnect'}
        </Button>
        {helperMissing && (
          <Button
            onClick={onOpenWorkspaceSettings}
            size="sm"
            type="button"
            variant="outline"
          >
            Open Workspace settings
          </Button>
        )}
      </div>
    </div>
  )
}

/**
 * What the Terminal view shows instead of a shell. Never blank: each state
 * names what is missing and offers the way forward.
 */
export function WorkspaceTerminalOnboarding({
  availability,
  onWake,
  onWakeOnConnect,
  onOpenWorkspaceSettings,
}: {
  availability: WorkspaceTerminalAvailability
  onWake: () => void
  onWakeOnConnect: () => void
  onOpenWorkspaceSettings: () => void
}) {
  switch (availability.kind) {
    case 'loading':
      return (
        <div
          aria-label="Checking the workspace sandbox"
          className="space-y-2"
          role="status"
        >
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-24 w-full" />
        </div>
      )
    case 'starting':
      return (
        <OnboardingCard
          busy
          detail="The persistent sandbox is being started. The terminal connects on its own as soon as it is ready."
          title="Starting the workspace sandbox…"
        />
      )
    case 'sleeping':
      return (
        <OnboardingCard
          action={
            <Button
              onClick={
                availability.wake === 'resume-action' ? onWake : onWakeOnConnect
              }
              size="sm"
              type="button"
            >
              <Power className="mr-1.5 size-3.5" />
              {availability.wake === 'resume-action'
                ? 'Wake workspace'
                : 'Wake and connect'}
            </Button>
          }
          detail="Temps suspends the sandbox when it is idle. Your files are kept; waking it takes a few seconds and then the terminal connects."
          title="The workspace sandbox is sleeping"
        />
      )
    case 'no-permission':
      return (
        <OnboardingCard
          action={
            <Button asChild size="sm" variant="outline">
              <Link to="/users">Manage users</Link>
            </Button>
          }
          detail={`A terminal runs commands inside the sandbox, which needs the sandboxes:exec permission. Your account has the ${availability.role} role, which does not include it. Ask an administrator to grant it, or use a role that does.`}
          title="Your account can't open a terminal"
        />
      )
    case 'runtime-update-required':
      return (
        <OnboardingCard
          action={
            <Button
              onClick={onOpenWorkspaceSettings}
              size="sm"
              type="button"
              variant="outline"
            >
              Open Workspace settings
            </Button>
          }
          detail={availability.detail}
          title="The sandbox runtime needs an update"
        />
      )
    case 'backend-unsupported':
      return (
        <OnboardingCard
          action={
            <Button asChild size="sm" variant="outline">
              <Link to="/agent-sandbox/sandbox">Open sandbox settings</Link>
            </Button>
          }
          detail={availability.reason}
          title="Terminals aren't available on this sandbox backend"
        >
          {availability.setupCommand && (
            <div className="flex items-center gap-2">
              <code className="min-w-0 overflow-x-auto whitespace-nowrap rounded bg-background px-2 py-1.5 font-mono text-[10px]">
                {availability.setupCommand}
              </code>
              <CopyButton
                className="size-7 shrink-0 rounded-md"
                label="Copy terminal setup command"
                minimal
                value={availability.setupCommand}
              />
            </div>
          )}
        </OnboardingCard>
      )
    case 'not-created':
      return (
        <OnboardingCard
          action={
            <Button onClick={onWakeOnConnect} size="sm" type="button">
              <Power className="mr-1.5 size-3.5" />
              Create and connect
            </Button>
          }
          detail="The workspace sandbox is created the first time it is used. Creating it starts a container and then opens the terminal; your files will persist across sleeps."
          title="The workspace sandbox hasn't been created yet"
        />
      )
    case 'failed':
      return (
        <OnboardingCard
          action={
            <Button
              onClick={onOpenWorkspaceSettings}
              size="sm"
              type="button"
              variant="outline"
            >
              Open Workspace
            </Button>
          }
          detail={availability.detail}
          title="The workspace sandbox isn't available"
        />
      )
    case 'ready':
      return null
  }
}

function OnboardingCard({
  title,
  detail,
  action,
  busy,
  children,
}: {
  title: string
  detail: string
  action?: ReactNode
  busy?: boolean
  children?: ReactNode
}) {
  return (
    <section
      aria-live="polite"
      className="space-y-3 rounded-xl border border-border bg-background p-3"
    >
      <div className="flex items-start gap-2">
        {busy && (
          <Loader2 className="mt-0.5 size-4 shrink-0 animate-spin text-muted-foreground" />
        )}
        <div className="min-w-0">
          <p className="text-sm font-medium">{title}</p>
          <p className="mt-1 text-xs leading-5 text-muted-foreground">
            {detail}
          </p>
        </div>
      </div>
      {children}
      <div className="rounded-lg bg-muted/40 px-2.5 py-2 font-mono text-[10px] leading-5 text-muted-foreground">
        <div>$ git status</div>
        <div>$ bun test</div>
        <div>$ claude</div>
      </div>
      {action}
    </section>
  )
}
