// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import type { ApplicationWorkspaceResponse } from '@/api/client'
import {
  INITIAL_TERMINAL_VIEW_STATE,
  TERMINAL_RECONNECT_MAX_ATTEMPTS,
  TERMINAL_STABLE_CONNECTION_MS,
  classifyTerminalFailure,
  reconnectDelayMs,
  roleLacksSandboxExec,
  terminalConnectTimeoutMs,
  terminalKeySequence,
  terminalViewBadge,
  terminalViewReducer,
  workspaceTerminalAvailability,
  workspaceTerminalSocketUrl,
  type TerminalViewEvent,
  type TerminalViewState,
} from './workspace-terminal'

function workspace(
  overrides: Partial<ApplicationWorkspaceResponse> = {}
): ApplicationWorkspaceResponse {
  return {
    cpu_limit: 1,
    data_network_service_count: 0,
    desired_state: 'running',
    disk_limit_enforced: false,
    disk_limit_mb: 1024,
    idle_timeout_secs: 600,
    memory_limit_mb: 1024,
    open_preview_ports: [],
    persistent_volume_healthy: true,
    pids_limit: 256,
    runtime: 'docker',
    runtime_update_available: false,
    sandbox_public_id: 'sbx_0123456789abcdef',
    state: 'running',
    ...overrides,
  }
}

const noJitter = () => 0.5

function run(events: TerminalViewEvent[], from = INITIAL_TERMINAL_VIEW_STATE) {
  return events.reduce<TerminalViewState>(
    (state, event) => terminalViewReducer(state, event),
    from
  )
}

const status = (
  value: 'connected' | 'closed' | 'failed' | 'exited',
  at: number,
  detail?: string
): TerminalViewEvent => ({ type: 'status', status: value, detail, at })

describe('workspaceTerminalSocketUrl', () => {
  const options = { thread: 'abc123', cols: 120, rows: 40 }

  test('uses the application workspace route, same origin', () => {
    expect(
      workspaceTerminalSocketUrl(
        { protocol: 'https:', host: 'temps.test' },
        'app_123',
        options
      )
    ).toBe(
      'wss://temps.test/api/ai/applications/app_123/workspace/terminal?thread=abc123&cols=120&rows=40'
    )
  })

  test('uses the global workspace route without an application', () => {
    expect(
      workspaceTerminalSocketUrl(
        { protocol: 'http:', host: 'localhost:3000' },
        null,
        options
      )
    ).toBe(
      'ws://localhost:3000/api/ai/workspace/terminal?thread=abc123&cols=120&rows=40'
    )
  })

  test('never puts a credential or a command in the URL', () => {
    const url = workspaceTerminalSocketUrl(
      { protocol: 'https:', host: 'temps.test' },
      'app_123',
      options
    )
    expect(url).not.toMatch(/token|key|secret|cmd|tab=/i)
  })

  test('encodes an application id that is not path safe', () => {
    expect(
      workspaceTerminalSocketUrl(
        { protocol: 'https:', host: 'temps.test' },
        'a/b?c',
        options
      )
    ).toContain('/applications/a%2Fb%3Fc/workspace/terminal')
  })
})

describe('reconnectDelayMs', () => {
  test('grows exponentially from one second and is capped', () => {
    const delays = [1, 2, 3, 4, 5, 6, 7, 20].map((n) =>
      reconnectDelayMs(n, noJitter)
    )
    expect(delays).toEqual([1000, 2000, 4000, 8000, 15000, 15000, 15000, 15000])
  })

  test('jitter stays within ±20% and never exceeds 18 seconds', () => {
    expect(reconnectDelayMs(1, () => 0)).toBe(800)
    expect(reconnectDelayMs(1, () => 1)).toBe(1200)
    expect(reconnectDelayMs(50, () => 1)).toBe(18000)
  })

  test('treats a nonsense attempt as the first', () => {
    expect(reconnectDelayMs(0, noJitter)).toBe(1000)
    expect(reconnectDelayMs(-3, noJitter)).toBe(1000)
    expect(reconnectDelayMs(Number.NaN, noJitter)).toBe(1000)
  })

  test('tolerates a misbehaving random source', () => {
    expect(reconnectDelayMs(1, () => 9)).toBe(1200)
    expect(reconnectDelayMs(1, () => -9)).toBe(800)
  })
})

describe('terminalViewReducer', () => {
  test('starts connecting and becomes connected, remembering a resume', () => {
    const state = run([
      {
        type: 'status',
        status: 'connected',
        detail: 'resumed',
        at: 100,
      },
    ])
    expect(state).toEqual({
      phase: 'connected',
      resumed: true,
      since: 100,
      attempt: 0,
    })
  })

  test('a refused first attach is reported, not retried', () => {
    const state = run([status('failed', 10, 'refused')])
    expect(state).toEqual({ phase: 'refused', reason: 'refused' })
    // It stays refused however many late events arrive.
    expect(run([status('closed', 20)], state).phase).toBe('refused')
  })

  test('a dropped connection reconnects with backoff, then resumes', () => {
    let state = run([status('connected', 0), status('closed', 60_000)])
    expect(state).toEqual({
      phase: 'reconnecting',
      attempt: 1,
      delayMs: 1000,
      reason: 'The connection was closed.',
    })
    state = run([{ type: 'reconnect-due' }], state)
    expect(state).toEqual({ phase: 'connecting', attempt: 1 })
    state = run([status('connected', 61_000, 'resumed')], state)
    expect(state.phase).toBe('connected')
    // Healthy again: the next drop starts the budget from scratch.
    expect(run([status('closed', 200_000)], state)).toMatchObject({
      phase: 'reconnecting',
      attempt: 1,
    })
  })

  test('failed reconnects back off and then give up as disconnected', () => {
    let state = run([status('connected', 0), status('closed', 60_000)])
    const delays: number[] = []
    for (let i = 0; i < 20 && state.phase === 'reconnecting'; i += 1) {
      delays.push(state.delayMs)
      state = run(
        [{ type: 'reconnect-due' }, status('failed', 70_000 + i, 'nope')],
        state
      )
    }
    expect(delays).toEqual([1000, 2000, 4000, 8000, 15000, 15000])
    expect(delays.length).toBe(TERMINAL_RECONNECT_MAX_ATTEMPTS)
    expect(state.phase).toBe('disconnected')
    expect(state).toMatchObject({
      reason: expect.stringContaining('Could not reconnect after 6 attempts'),
    })
  })

  test('a connection that drops straight away does not reset the budget', () => {
    let state: TerminalViewState = { phase: 'connecting', attempt: 0 }
    let at = 0
    let rounds = 0
    state = run([status('connected', at)], state)
    while (state.phase !== 'disconnected' && rounds < 50) {
      rounds += 1
      at += 100 // up for 100 ms, far below the stable threshold
      state = run([status('closed', at)], state)
      if (state.phase === 'reconnecting') {
        at += state.delayMs
        state = run(
          [{ type: 'reconnect-due' }, status('connected', at, 'resumed')],
          state
        )
      }
    }
    expect(state.phase).toBe('disconnected')
    expect(rounds).toBeLessThanOrEqual(TERMINAL_RECONNECT_MAX_ATTEMPTS + 1)
  })

  test('a stable connection earns a fresh budget', () => {
    const state = run([
      status('connected', 0),
      status('closed', TERMINAL_STABLE_CONNECTION_MS),
    ])
    expect(state).toMatchObject({ phase: 'reconnecting', attempt: 1 })
  })

  test('a server error after ready is treated as a drop, not a refusal', () => {
    const state = run([
      status('connected', 0),
      status('failed', 1000, 'terminal session reached its maximum duration'),
    ])
    expect(state).toMatchObject({
      phase: 'reconnecting',
      reason: 'terminal session reached its maximum duration',
    })
  })

  test('the shell exiting is final and is not reconnected', () => {
    const exited = run([
      status('connected', 0),
      status('exited', 5, 'The shell exited with code 0.'),
    ])
    expect(exited).toEqual({
      phase: 'exited',
      reason: 'The shell exited with code 0.',
    })
    // The socket closing after the exit event must not start a reconnect.
    expect(run([status('closed', 6)], exited)).toEqual(exited)
  })

  test('manual retry resets everything', () => {
    const disconnected: TerminalViewState = {
      phase: 'disconnected',
      reason: 'x',
    }
    expect(run([{ type: 'retry' }], disconnected)).toEqual({
      phase: 'connecting',
      attempt: 0,
    })
    // ...so a refusal after the retry is a first-attempt refusal again.
    expect(
      run([{ type: 'retry' }, status('failed', 1, 'still no')], disconnected)
    ).toEqual({ phase: 'refused', reason: 'still no' })
  })

  test('a due timer outside reconnecting is ignored', () => {
    const state: TerminalViewState = {
      phase: 'connected',
      resumed: false,
      since: 0,
      attempt: 0,
    }
    expect(run([{ type: 'reconnect-due' }], state)).toBe(state)
  })
})

describe('roleLacksSandboxExec', () => {
  test('flags roles known to lack the permission only', () => {
    for (const role of ['platform_admin', 'reader', 'api_reader']) {
      expect(roleLacksSandboxExec(role)).toBe(true)
    }
    for (const role of ['admin', 'user', 'custom', 'something-new']) {
      expect(roleLacksSandboxExec(role)).toBe(false)
    }
    expect(roleLacksSandboxExec(undefined)).toBe(false)
    expect(roleLacksSandboxExec(null)).toBe(false)
  })
})

describe('workspaceTerminalAvailability', () => {
  const base = {
    workspace: workspace(),
    loading: false,
    waking: false,
    hasApplication: true,
    role: 'user',
  }

  test('a running workspace is ready with its sandbox id', () => {
    expect(workspaceTerminalAvailability(base)).toEqual({
      kind: 'ready',
      sandboxPublicId: 'sbx_0123456789abcdef',
    })
  })

  test('says what is missing for a role without sandboxes:exec', () => {
    expect(
      workspaceTerminalAvailability({ ...base, role: 'platform_admin' })
    ).toEqual({ kind: 'no-permission', role: 'platform_admin' })
    // The permission check wins over a sleeping sandbox: waking it is moot.
    expect(
      workspaceTerminalAvailability({
        ...base,
        role: 'reader',
        workspace: workspace({ state: 'sleeping' }),
      }).kind
    ).toBe('no-permission')
  })

  test('an incompatible runtime needs an update first', () => {
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: workspace({
          runtime_compatible: false,
          runtime_update_error: 'image too old',
        }),
      })
    ).toEqual({ kind: 'runtime-update-required', detail: 'image too old' })
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: workspace({ runtime_compatible: false }),
      }).kind
    ).toBe('runtime-update-required')
  })

  test('a sleeping workspace is woken by the action or on connect', () => {
    const sleeping = workspace({ state: 'sleeping' })
    expect(
      workspaceTerminalAvailability({ ...base, workspace: sleeping })
    ).toEqual({ kind: 'sleeping', wake: 'resume-action' })
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: sleeping,
        hasApplication: false,
      })
    ).toEqual({ kind: 'sleeping', wake: 'on-connect' })
  })

  test('waking and recovering both read as starting', () => {
    expect(
      workspaceTerminalAvailability({
        ...base,
        waking: true,
        workspace: workspace({ state: 'sleeping' }),
      }).kind
    ).toBe('starting')
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: workspace({ state: 'recovering' }),
      }).kind
    ).toBe('starting')
  })

  test('loading, missing and failed workspaces are described', () => {
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: null,
        loading: true,
      }).kind
    ).toBe('loading')
    expect(
      workspaceTerminalAvailability({ ...base, workspace: null }).kind
    ).toBe('failed')
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: workspace({ state: 'failed', last_error: 'disk full' }),
      })
    ).toEqual({ kind: 'failed', detail: 'disk full' })
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: workspace({ sandbox_public_id: null, state: 'sleeping' }),
      }).kind
    ).toBe('no-sandbox')
  })

  test('a backend that reports no terminal support explains why', () => {
    expect(
      workspaceTerminalAvailability({
        ...base,
        workspace: workspace({ runtime: 'firecracker' }),
        backendTerminal: {
          supported: false,
          reason: 'the guest terminal agent is not staged',
          setup_command: 'temps firecracker setup --pty-agent-bin <path>',
        },
      })
    ).toEqual({
      kind: 'backend-unsupported',
      reason: 'the guest terminal agent is not staged',
      setupCommand: 'temps firecracker setup --pty-agent-bin <path>',
    })
    // No report means: try it and let the endpoint answer.
    expect(
      workspaceTerminalAvailability({ ...base, backendTerminal: null }).kind
    ).toBe('ready')
    expect(
      workspaceTerminalAvailability({
        ...base,
        backendTerminal: { supported: true },
      }).kind
    ).toBe('ready')
  })
})

describe('terminalConnectTimeoutMs', () => {
  test('gives a wake far longer than an ordinary attach', () => {
    expect(terminalConnectTimeoutMs(false)).toBe(15_000)
    expect(terminalConnectTimeoutMs(true)).toBeGreaterThan(60_000)
  })
})

describe('terminalKeySequence', () => {
  test('control keys match a physical keyboard', () => {
    expect(terminalKeySequence('escape')).toBe('\u001b')
    expect(terminalKeySequence('tab')).toBe('\t')
    expect(terminalKeySequence('ctrl-c')).toBe('\u0003')
  })

  test('arrows follow the cursor key mode of the running program', () => {
    expect(terminalKeySequence('up')).toBe('\u001b[A')
    expect(terminalKeySequence('left')).toBe('\u001b[D')
    expect(terminalKeySequence('up', true)).toBe('\u001bOA')
    expect(terminalKeySequence('right', true)).toBe('\u001bOC')
  })
})

describe('reconnect jitter', () => {
  test('uses the roll supplied with the event, keeping the reducer pure', () => {
    const event = (roll: number): TerminalViewEvent => ({
      type: 'status',
      status: 'closed',
      at: 0,
      roll,
    })
    const connected: TerminalViewState = {
      phase: 'connected',
      resumed: false,
      since: 0,
      attempt: 0,
    }
    expect(terminalViewReducer(connected, event(0))).toMatchObject({
      delayMs: 800,
    })
    expect(terminalViewReducer(connected, event(1))).toMatchObject({
      delayMs: 1200,
    })
    // Same input, same output.
    expect(terminalViewReducer(connected, event(0.3))).toEqual(
      terminalViewReducer(connected, event(0.3))
    )
  })
})

describe('terminalViewBadge', () => {
  test('every lost-connection state says it is disconnected or failed', () => {
    expect(
      terminalViewBadge({
        phase: 'reconnecting',
        attempt: 2,
        delayMs: 2000,
        reason: 'x',
      }).label
    ).toBe('Disconnected · retrying in 2s (2/6)')
    expect(terminalViewBadge({ phase: 'disconnected', reason: 'x' })).toEqual({
      label: 'Disconnected',
      tone: 'danger',
    })
    expect(terminalViewBadge({ phase: 'refused', reason: 'x' }).label).toBe(
      'Could not connect'
    )
  })

  test('distinguishes first connect, reconnect and a resumed session', () => {
    expect(terminalViewBadge({ phase: 'connecting', attempt: 0 }).label).toBe(
      'Connecting…'
    )
    expect(terminalViewBadge({ phase: 'connecting', attempt: 3 }).label).toBe(
      'Reconnecting…'
    )
    expect(
      terminalViewBadge({
        phase: 'connected',
        resumed: true,
        since: 0,
        attempt: 0,
      })
    ).toEqual({ label: 'Connected · session resumed', tone: 'success' })
  })
})

describe('classifyTerminalFailure', () => {
  test('recognises a sandbox whose terminal helper did not start', () => {
    // The relay's launch error is read as a binary frame header: the first
    // four bytes of "OCI runtime exec failed" decode to this length.
    expect(
      classifyTerminalFailure(
        'terminal stream failed: frame too large: 1329809696 bytes'
      )
    ).toBe('helper-missing')
    expect(
      classifyTerminalFailure('PTY agent relay error: socat: command not found')
    ).toBe('helper-missing')
    expect(
      classifyTerminalFailure(
        'exec: "socat": executable file not found in $PATH'
      )
    ).toBe('helper-missing')
    expect(
      classifyTerminalFailure('connect to /run/temps-pty/agent.sock failed')
    ).toBe('helper-missing')
  })

  test('leaves unrelated failures unclassified', () => {
    expect(classifyTerminalFailure('')).toBe('unknown')
    expect(classifyTerminalFailure('The connection was closed.')).toBe(
      'unknown'
    )
    expect(
      classifyTerminalFailure('Forbidden: sandboxes:exec is required')
    ).toBe('unknown')
  })
})
