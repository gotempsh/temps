// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Policy for the AI workspace terminal: which tab it attaches to, when it may
 * attach, and how it recovers when the connection drops.
 *
 * Everything here is pure so the rules can be tested without a socket or a
 * DOM. The socket protocol itself lives in
 * `@/components/sandboxes/sandbox-terminal`, which this view shares with the
 * standalone sandbox page; nothing here talks to a backend directly, so it
 * works on whichever sandbox backend implements `attach_pty`.
 */

import type { ApplicationWorkspaceResponse } from '@/api/client'
import type { TerminalStatus } from '@/components/sandboxes/sandbox-terminal'

/**
 * Socket URL of a terminal thread.
 *
 * Not `/v1/sandboxes/{id}/terminal`: managed workspaces are hidden from the
 * generic sandbox API on purpose (every access must re-check the linked
 * projects), so the terminal is served under the workspace's own routes. The
 * thread names the shell; the server derives its PTY tab from the thread, so
 * the client never picks one. Same origin as the console, so the session
 * cookie authenticates it and nothing secret goes in the URL.
 */
export function workspaceTerminalSocketUrl(
  location: Pick<Location, 'protocol' | 'host'>,
  applicationPublicId: string | null,
  options: { thread: string; cols: number; rows: number }
): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
  const params = new URLSearchParams({
    thread: options.thread,
    cols: String(options.cols),
    rows: String(options.rows),
  })
  const path = applicationPublicId
    ? `/api/ai/applications/${encodeURIComponent(applicationPublicId)}/workspace/terminal`
    : '/api/ai/workspace/terminal'
  return `${protocol}//${location.host}${path}?${params.toString()}`
}

/** Reconnect attempts allowed after a connection drops before giving up. */
export const TERMINAL_RECONNECT_MAX_ATTEMPTS = 6

const RECONNECT_BASE_MS = 1_000
const RECONNECT_CAP_MS = 15_000
/**
 * A connection must stay up this long before the attempt counter resets. A
 * socket that opens and is dropped straight away would otherwise reset the
 * budget every time and reconnect forever.
 */
export const TERMINAL_STABLE_CONNECTION_MS = 10_000

/**
 * Delay before reconnect attempt `attempt` (1-based): exponential from one
 * second, capped, with ±20% jitter so a server restart does not bring every
 * open terminal back at the same instant. Never above 18 seconds.
 */
export function reconnectDelayMs(
  attempt: number,
  random: () => number = Math.random
): number {
  const step = Number.isFinite(attempt) ? Math.max(1, Math.floor(attempt)) : 1
  const base = Math.min(RECONNECT_CAP_MS, RECONNECT_BASE_MS * 2 ** (step - 1))
  const roll = random()
  const jitter =
    0.8 + 0.4 * (Number.isFinite(roll) ? Math.min(1, Math.max(0, roll)) : 0.5)
  return Math.round(base * jitter)
}

export type TerminalViewState =
  /** `attempt` is 0 for the first connection, then counts reconnects. */
  | { phase: 'connecting'; attempt: number }
  | { phase: 'connected'; resumed: boolean; since: number; attempt: number }
  | { phase: 'reconnecting'; attempt: number; delayMs: number; reason: string }
  /** Retries exhausted: the user decides. Always says it is disconnected. */
  | { phase: 'disconnected'; reason: string }
  | { phase: 'exited'; reason: string }
  /** The very first attach was refused; retrying on its own will not help. */
  | { phase: 'refused'; reason: string }

export type TerminalViewEvent =
  | {
      type: 'status'
      status: TerminalStatus
      detail?: string
      /** Event time in ms; used to tell a stable connection from a flap. */
      at: number
      /** Jitter source in [0, 1), supplied by the caller to keep this pure. */
      roll?: number
    }
  /** The user pressed Reconnect / Try again: a fresh attempt budget. */
  | { type: 'retry' }
  /** The backoff timer for a `reconnecting` state elapsed. */
  | { type: 'reconnect-due' }

export const INITIAL_TERMINAL_VIEW_STATE: TerminalViewState = {
  phase: 'connecting',
  attempt: 0,
}

export function terminalViewReducer(
  state: TerminalViewState,
  event: TerminalViewEvent
): TerminalViewState {
  switch (event.type) {
    case 'retry':
      return { phase: 'connecting', attempt: 0 }
    case 'reconnect-due':
      return state.phase === 'reconnecting'
        ? { phase: 'connecting', attempt: state.attempt }
        : state
    case 'status':
      return reduceStatus(state, event)
  }
}

function reduceStatus(
  state: TerminalViewState,
  event: Extract<TerminalViewEvent, { type: 'status' }>
): TerminalViewState {
  const { status, detail, at, roll } = event
  switch (status) {
    case 'idle':
    case 'connecting':
      return state
    case 'connected':
      return {
        phase: 'connected',
        resumed: detail === 'resumed',
        since: at,
        attempt: state.phase === 'connecting' ? state.attempt : 0,
      }
    case 'exited':
      return {
        phase: 'exited',
        reason: detail ?? 'The shell exited.',
      }
    case 'closed':
    case 'failed': {
      const reason =
        detail ??
        (status === 'closed'
          ? 'The connection was closed.'
          : 'The terminal could not connect.')
      if (state.phase === 'connecting' && state.attempt === 0) {
        // Never reached `ready`. The causes (permission, a sandbox that
        // cannot be attached, a missing PTY agent) do not heal by retrying.
        return { phase: 'refused', reason }
      }
      if (
        state.phase === 'refused' ||
        state.phase === 'disconnected' ||
        state.phase === 'exited'
      ) {
        return state
      }
      let attempt = state.attempt + 1
      if (state.phase === 'connected') {
        const stable = at - state.since >= TERMINAL_STABLE_CONNECTION_MS
        attempt = stable ? 1 : state.attempt + 1
      }
      if (attempt > TERMINAL_RECONNECT_MAX_ATTEMPTS) {
        return {
          phase: 'disconnected',
          reason: `Could not reconnect after ${TERMINAL_RECONNECT_MAX_ATTEMPTS} attempts. ${reason}`,
        }
      }
      return {
        phase: 'reconnecting',
        attempt,
        delayMs: reconnectDelayMs(attempt, () => roll ?? 0.5),
        reason,
      }
    }
  }
}

/**
 * Roles the console knows to lack `sandboxes:exec` (mirrors
 * `Role::permissions` in `temps-auth`). Anything else, including custom or
 * unknown roles, is let through: the endpoint enforces the permission, and a
 * wrong client guess must never lock out someone who is allowed.
 */
const ROLES_WITHOUT_SANDBOX_EXEC = new Set([
  'platform_admin',
  'reader',
  'api_reader',
  'metrics_ingest',
])

export function roleLacksSandboxExec(role: string | null | undefined): boolean {
  return role != null && ROLES_WITHOUT_SANDBOX_EXEC.has(role)
}

/**
 * Terminal capability reported for a backend (the shape PR gotempsh/temps#1480
 * adds to the sandbox status API). Optional here: when a backend does not
 * report one, the view assumes the backend can attach and lets the endpoint
 * refuse.
 */
export interface BackendTerminalSupport {
  supported: boolean
  reason?: string | null
  setup_command?: string | null
}

export type WorkspaceTerminalAvailability =
  /** The workspace status has not loaded yet. */
  | { kind: 'loading' }
  | { kind: 'no-permission'; role: string }
  | { kind: 'runtime-update-required'; detail: string }
  | { kind: 'backend-unsupported'; reason: string; setupCommand: string | null }
  /**
   * No sandbox exists yet. Attaching creates it, so connecting is the action;
   * it is the user's to take because it starts a container.
   */
  | { kind: 'not-created' }
  | { kind: 'failed'; detail: string }
  /** A wake is in flight, or the sandbox is coming up on its own. */
  | { kind: 'starting' }
  /**
   * Suspended. `resume-action` wakes it through the application workspace
   * control action; `on-connect` is the global workspace, which has no
   * control action and is woken by the terminal attach itself.
   */
  | { kind: 'sleeping'; wake: 'resume-action' | 'on-connect' }
  | { kind: 'ready'; sandboxPublicId: string }

export function workspaceTerminalAvailability(input: {
  workspace: ApplicationWorkspaceResponse | null
  loading: boolean
  waking: boolean
  hasApplication: boolean
  role: string | null | undefined
  backendTerminal?: BackendTerminalSupport | null
}): WorkspaceTerminalAvailability {
  const { workspace, loading, waking, hasApplication, role } = input
  if (roleLacksSandboxExec(role)) {
    return { kind: 'no-permission', role: role ?? 'unknown' }
  }
  if (workspace?.runtime_compatible === false) {
    return {
      kind: 'runtime-update-required',
      detail:
        workspace.runtime_update_error ??
        'The sandbox runtime must be updated before it can host a terminal.',
    }
  }
  if (waking) return { kind: 'starting' }
  if (!workspace) {
    return loading
      ? { kind: 'loading' }
      : {
          kind: 'failed',
          detail: 'The workspace status could not be loaded.',
        }
  }
  if (workspace.state === 'failed') {
    return {
      kind: 'failed',
      detail: workspace.last_error ?? 'The workspace sandbox failed to start.',
    }
  }
  const sandboxPublicId = workspace.sandbox_public_id
  if (!sandboxPublicId) {
    // Nothing has been created yet. The attach route creates the sandbox on
    // first use, so there is no id to wait for before connecting.
    return workspace.state === 'recovering'
      ? { kind: 'starting' }
      : { kind: 'not-created' }
  }
  const backend = input.backendTerminal
  if (backend && !backend.supported) {
    return {
      kind: 'backend-unsupported',
      reason:
        backend.reason ??
        `The ${workspace.runtime} sandbox backend cannot open terminals.`,
      setupCommand: backend.setup_command ?? null,
    }
  }
  if (workspace.state === 'recovering') return { kind: 'starting' }
  if (workspace.state === 'running') {
    return { kind: 'ready', sandboxPublicId }
  }
  return {
    kind: 'sleeping',
    wake: hasApplication ? 'resume-action' : 'on-connect',
  }
}

/**
 * How long to wait for the socket to report `ready`. A sleeping workspace is
 * woken by the attach itself, which can take a container start, so it gets
 * far longer than a running one (the server bounds the wake on its side).
 */
export function terminalConnectTimeoutMs(woken: boolean): number {
  return woken ? 90_000 : 15_000
}

/**
 * Keys a phone's on-screen keyboard cannot type but a shell needs: leaving a
 * prompt (Esc), completing (Tab), interrupting (Ctrl-C), history and cursor
 * movement.
 */
export type TerminalKey =
  'escape' | 'tab' | 'ctrl-c' | 'up' | 'down' | 'left' | 'right'

const ARROWS = { up: 'A', down: 'B', right: 'C', left: 'D' } as const

/**
 * Bytes a physical key press would have produced. Arrows follow the cursor
 * key mode the running program selected (vim, less and readline switch to
 * application mode), as a real keyboard in xterm does.
 */
export function terminalKeySequence(
  key: TerminalKey,
  applicationCursorKeys = false
): string {
  switch (key) {
    case 'escape':
      return '\x1b'
    case 'tab':
      return '\t'
    case 'ctrl-c':
      return '\x03'
    default:
      return `\x1b${applicationCursorKeys ? 'O' : '['}${ARROWS[key]}`
  }
}

export type TerminalFailureCause = 'helper-missing' | 'unknown'

/**
 * Failures that mean the sandbox's PTY helper (`socat` relaying to the
 * in-sandbox agent socket) did not start. The relay's own launch error is
 * read back as the first protocol frame, so the symptom is usually an absurd
 * "frame too large" rather than a message naming the helper.
 */
const HELPER_MISSING_SIGNATURES = [
  /frame too large/i,
  /socat/i,
  /agent\.sock/i,
  /pty agent/i,
  /executable file not found/i,
  /oci runtime exec failed/i,
]

/**
 * Names the likely cause of a refused or lost terminal from the server's
 * message. A pure string match: the message is the only thing a browser
 * WebSocket can see of the failure.
 */
export function classifyTerminalFailure(reason: string): TerminalFailureCause {
  return HELPER_MISSING_SIGNATURES.some((signature) => signature.test(reason))
    ? 'helper-missing'
    : 'unknown'
}

export const TERMINAL_HELPER_MISSING_HINT =
  "This sandbox's runtime image does not include the terminal helper (the PTY agent and socat), so no shell can be attached to it. It needs a runtime image that ships them: update the Temps sandbox images, then use Update runtime in Workspace settings. Workspace files are preserved."

export type TerminalBadgeTone = 'success' | 'warning' | 'neutral' | 'danger'

/** The short status the toolbar shows; always names a lost connection. */
export function terminalViewBadge(view: TerminalViewState): {
  label: string
  tone: TerminalBadgeTone
} {
  switch (view.phase) {
    case 'connecting':
      return {
        label: view.attempt === 0 ? 'Connecting…' : 'Reconnecting…',
        tone: 'neutral',
      }
    case 'connected':
      return {
        label: view.resumed ? 'Connected · session resumed' : 'Connected',
        tone: 'success',
      }
    case 'reconnecting':
      return {
        label: `Disconnected · retrying in ${Math.max(1, Math.ceil(view.delayMs / 1000))}s (${view.attempt}/${TERMINAL_RECONNECT_MAX_ATTEMPTS})`,
        tone: 'warning',
      }
    case 'disconnected':
      return { label: 'Disconnected', tone: 'danger' }
    case 'exited':
      return { label: 'Session ended', tone: 'neutral' }
    case 'refused':
      return { label: 'Could not connect', tone: 'danger' }
  }
}
