// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps ai provider login <id>` — sign an AI CLI (Claude Code, Codex) in to a
 * subscription from the terminal.
 *
 * The Temps server drives the provider CLI itself; this command only relays
 * what the operator has to do (open a link, enter a device code, paste a code)
 * and reports progress. The pasted code is never echoed or printed back.
 */

export type LoginPhase =
  | 'starting'
  | 'waiting'
  | 'verifying'
  | 'succeeded'
  | 'failed'
  | 'cancelled'

export interface LoginSession {
  session_id: string
  provider_id: string
  phase: LoginPhase
  detail: string
  verification_url?: string | null
  user_code?: string | null
  requires_code: boolean
  elapsed_seconds: number
  timeout_seconds: number
}

/** The four server operations the flow needs. */
export interface ProviderLoginApi {
  start(providerId: string): Promise<LoginSession>
  status(providerId: string): Promise<LoginSession>
  submitCode(providerId: string, code: string): Promise<LoginSession>
  cancel(providerId: string): Promise<LoginSession>
}

/** Everything that touches the terminal or the clock, so the flow is testable. */
export interface LoginIo {
  log(line: string): void
  /** Ask for the code the provider shows after approval. Must not echo it. */
  promptCode(message: string): Promise<string>
  sleep(ms: number): Promise<void>
}

export interface LoginFlowOptions {
  pollIntervalMs?: number
  /** Called once with a function that cancels the sign-in, e.g. on Ctrl-C. */
  onCancelable?: (cancel: () => Promise<void>) => void
  /**
   * True when the user asked to stop before the server answered `start`. The
   * sign-in is then cancelled as soon as it exists instead of being left
   * running (and blocking a retry) until its server-side timeout.
   */
  shouldAbort?: () => boolean
}

const TERMINAL: ReadonlySet<LoginPhase> = new Set([
  'succeeded',
  'failed',
  'cancelled',
])

/**
 * Where human-readable progress goes. With `--json` stdout is reserved for the
 * final JSON document, so the link, progress and prompts move to stderr.
 */
export function progressLogger(
  json: boolean | undefined,
  stdout: (line: string) => void = (line) => console.log(line),
  stderr: (line: string) => void = (line) => console.error(line)
): (line: string) => void {
  return json ? stderr : stdout
}

export class LoginFlowError extends Error {
  constructor(
    message: string,
    readonly phase: LoginPhase
  ) {
    super(message)
    this.name = 'LoginFlowError'
  }
}

/**
 * Stop the sign-in the user walked away from, and say so when that did not
 * work. A failed cancel leaves the server-side sign-in running until its
 * timeout, holding the provider's single slot, so the user has to be told how
 * to clear it rather than being shown a clean "cancelled".
 */
async function cancelAndExplain(
  api: ProviderLoginApi,
  providerId: string,
  reason: string,
  timeoutSeconds: number
): Promise<never> {
  let failure: unknown
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      await api.cancel(providerId)
      throw new LoginFlowError(reason, 'cancelled')
    } catch (error) {
      if (error instanceof LoginFlowError) throw error
      failure = error
    }
  }
  const why = failure instanceof Error ? failure.message : String(failure)
  throw new LoginFlowError(
    `${reason} The server could not be told to stop it (${why}), so it may keep running for up to ${Math.round(timeoutSeconds / 60)} minutes and block another attempt. ` +
      `Cancel it from Settings → AI Agents, or run \`temps ai provider login ${providerId}\` again to resume it.`,
    'cancelled'
  )
}

function describe(session: LoginSession): string {
  return `${session.phase} (${session.elapsed_seconds}s of ${session.timeout_seconds}s)`
}

/**
 * Run one sign-in to completion. Resolves with the final session when it
 * succeeded and throws {@link LoginFlowError} when it failed or was cancelled.
 */
export async function runProviderLogin(
  api: ProviderLoginApi,
  io: LoginIo,
  providerId: string,
  options: LoginFlowOptions = {}
): Promise<LoginSession> {
  const pollIntervalMs = options.pollIntervalMs ?? 1000
  let session = await api.start(providerId)
  options.onCancelable?.(async () => {
    await api.cancel(providerId)
  })
  if (options.shouldAbort?.()) {
    return cancelAndExplain(
      api,
      providerId,
      'Sign-in cancelled.',
      session.timeout_seconds
    )
  }

  let announcedLink = false
  let submitted = false
  let lastPhase: LoginPhase | undefined

  while (!TERMINAL.has(session.phase)) {
    if (session.phase !== lastPhase) {
      lastPhase = session.phase
      if (session.phase === 'starting') io.log('Starting the sign-in…')
      if (session.phase === 'verifying') io.log(`Verifying… ${session.detail}`)
    }

    if (session.phase === 'waiting' && session.verification_url) {
      if (!announcedLink) {
        announcedLink = true
        io.log('')
        io.log('Open this link in a browser and approve access:')
        io.log(`  ${session.verification_url}`)
        if (session.user_code) {
          io.log('')
          io.log(`Then enter this code: ${session.user_code}`)
        }
        io.log('')
      }
      if (session.requires_code && !submitted) {
        const code = (
          await io.promptCode('Paste the code shown after approving access')
        ).trim()
        if (!code) {
          return cancelAndExplain(
            api,
            providerId,
            'No code entered; sign-in cancelled.',
            session.timeout_seconds
          )
        }
        submitted = true
        session = await api.submitCode(providerId, code)
        continue
      }
      if (!session.requires_code && !submitted) {
        submitted = true
        io.log('Waiting for you to finish in the browser…')
      }
    }

    await io.sleep(pollIntervalMs)
    session = await api.status(providerId)
  }

  if (session.phase === 'succeeded') {
    io.log(session.detail || 'Signed in.')
    return session
  }
  throw new LoginFlowError(
    session.detail || `Sign-in ${describe(session)}`,
    session.phase
  )
}
