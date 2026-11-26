// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { credentials, config } from '../../config/store.js'
import { upsertContext, defaultContextName, getContext } from '../../config/contexts.js'
import { promptPassword } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, icons, colors, newline, box } from '../../ui/output.js'
import {
  setupClient,
  client,
  ApiResponseError,
  isUserObject,
} from '../../lib/api-client.js'
import { getCurrentUser } from '../../api/sdk.gen.js'
import { AuthenticationError, CliError } from '../../utils/errors.js'
import { hostname, platform } from 'node:os'
import { spawn } from 'node:child_process'
import {
  clearPendingLogin,
  getPendingLogin,
  pendingLoginExpired,
  savePendingLogin,
  type PendingLogin,
} from './pending-login.js'

interface LoginOptions {
  /** Pre-minted API key from the dashboard's Settings → API Keys page. Use this for headless / CI. */
  apiKey?: string
  /** Optional friendly name for the saved context (defaults to URL host). */
  context?: string
  /** Override the server URL for this login (otherwise uses config / active context). */
  url?: string
  /** Emit verbose request/response logging for diagnosing connection issues. */
  debug?: boolean
  /**
   * `false` (`--no-wait`): print the approval link and exit instead of
   * waiting. Finish with `--resume`.
   */
  wait?: boolean
  /** Finish a sign-in started with `--no-wait`. */
  resume?: boolean
  /** With `--resume`: seconds to wait for the approval before giving up. */
  waitSecs?: string
}

/** How long `--resume` waits by default: under a typical two-minute tool timeout. */
export const DEFAULT_RESUME_WAIT_SECS = 90

/** Exit code of `--resume` when the person has not approved yet. */
export const LOGIN_STILL_PENDING_EXIT_CODE = 2

export type DeviceLoginMode = 'wait' | 'start' | 'resume'

/**
 * Which half of the browser sign-in this invocation runs. `--no-wait` and
 * `--resume` split it for callers that cannot keep a command open while a
 * person approves: an agent's shell tool returns output only when the
 * command exits, so a login that prints the link and then blocks never shows
 * the link to anyone.
 */
export function resolveDeviceLoginMode(
  options: Pick<LoginOptions, 'wait' | 'resume' | 'apiKey'>,
): DeviceLoginMode {
  const start = options.wait === false
  const resume = options.resume === true
  if (start && resume) {
    throw new AuthenticationError(
      '--no-wait and --resume cannot be combined: --no-wait prints the approval link and exits, --resume waits for that approval.',
    )
  }
  if ((start || resume) && options.apiKey !== undefined) {
    throw new AuthenticationError(
      `${start ? '--no-wait' : '--resume'} applies to the browser sign-in only; an --api-key login needs no approval.`,
    )
  }
  if (start) return 'start'
  return resume ? 'resume' : 'wait'
}

/** Parse `--wait-secs`; a missing value is the default. */
export function parseResumeWaitSecs(value: string | undefined): number {
  if (value === undefined) return DEFAULT_RESUME_WAIT_SECS
  if (!/^[1-9]\d*$/.test(value.trim())) {
    throw new AuthenticationError(
      `--wait-secs must be a whole number of seconds greater than zero, got "${value}".`,
    )
  }
  return Number(value.trim())
}

/**
 * Whether verbose request/response logging is enabled for this invocation.
 * Activated by `--debug` on the command or `TEMPS_DEBUG=1` in the environment.
 */
export function debugEnabled(opts: { debug?: boolean } = {}): boolean {
  if (opts.debug) return true
  const env = process.env.TEMPS_DEBUG
  return env === '1' || env === 'true' || env === 'yes'
}

function debugLog(message: string, payload?: unknown): void {
  if (payload === undefined) {
    process.stderr.write(`[temps-cli debug] ${message}\n`)
    return
  }
  let rendered: string
  try {
    rendered = typeof payload === 'string' ? payload : JSON.stringify(payload, null, 2)
  } catch {
    rendered = String(payload)
  }
  process.stderr.write(`[temps-cli debug] ${message} ${rendered}\n`)
}

/**
 * Wraps `fetch` so debug mode can see exactly what was sent and what came back.
 * We read the raw body as text first, log it, and then re-parse as JSON so the
 * caller still gets a parsed object (or a typed error pointing at the raw body).
 */
/**
 * `work`, or a rejection with the signal's reason as soon as `signal` aborts,
 * whichever comes first. Bounds a body read the same way `fetch` bounds the
 * request, whatever the runtime does with the signal once headers arrived.
 */
function untilAborted<T>(work: Promise<T>, signal: AbortSignal | null | undefined): Promise<T> {
  if (!signal) return work
  if (signal.aborted) return Promise.reject(signal.reason)
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(signal.reason)
    signal.addEventListener('abort', onAbort, { once: true })
    work.then(
      (value) => {
        signal.removeEventListener('abort', onAbort)
        resolve(value)
      },
      (err) => {
        signal.removeEventListener('abort', onAbort)
        reject(err)
      },
    )
  })
}

async function debugFetch(
  url: string,
  init: RequestInit,
  debug: boolean,
): Promise<{ res: Response; rawBody: string; json: unknown }> {
  if (debug) {
    debugLog(`-> ${init.method ?? 'GET'} ${url}`)
    if (init.body) debugLog('   request body:', init.body)
  }
  let res: Response
  try {
    res = await untilAborted(fetch(url, init), init.signal)
  } catch (err) {
    // The caller's deadline, not an unreachable server: let it say so.
    if (init.signal?.aborted) throw err
    const reason = err instanceof Error ? err.message : String(err)
    if (debug) debugLog(`   fetch failed: ${reason}`)
    throw new AuthenticationError(
      `Unable to connect to ${url}: ${reason}. Is the server reachable from this machine?`,
    )
  }
  const rawBody = await untilAborted(res.text(), init.signal)
  if (debug) {
    debugLog(`<- ${res.status} ${res.statusText} (${url})`)
    const headers: Record<string, string> = {}
    res.headers.forEach((value, key) => {
      headers[key] = value
    })
    debugLog('   response headers:', headers)
    const preview = rawBody.length > 2000 ? `${rawBody.slice(0, 2000)}…[truncated]` : rawBody
    debugLog('   response body:', preview || '(empty)')
  }
  let json: unknown = null
  if (rawBody.length > 0) {
    try {
      json = JSON.parse(rawBody)
    } catch {
      json = null
    }
  }
  return { res, rawBody, json }
}

/**
 * The device-auth endpoints are served by the auth plugin, which is mounted
 * under `/api` by the core router (see `temps-core/src/plugin.rs:760`). So
 * the real URLs are `/api/auth/cli/device/start` and `…/poll`.
 *
 * Users may pass:
 *   - bare host: `https://app.temps.kfs.es`           -> add `/api`
 *   - with prefix: `https://app.temps.kfs.es/api`     -> keep as-is
 *   - with trailing slash: `https://app.temps.kfs.es/` -> normalize then add `/api`
 *
 * Returns the `/api`-suffixed base, with no trailing slash.
 */
export function serverBaseUrl(rawApiUrl: string): string {
  const trimmed = rawApiUrl.replace(/\/+$/, '')
  return /\/api$/.test(trimmed) ? trimmed : `${trimmed}/api`
}

/**
 * Pick how `temps login` authenticates. An `--api-key` that was passed but is
 * empty (`--api-key "$TEMPS_API_KEY"` with the variable unset, because the
 * line that should have read it failed) is an error, not a request for the
 * browser flow: falling through used to mint a separate `cli-device:` key and
 * log in anyway, so the key the caller meant to use was silently never used.
 */
export function resolveLoginMethod(
  options: Pick<LoginOptions, 'apiKey'>,
): 'api-key' | 'device' {
  if (options.apiKey === undefined) return 'device'
  if (options.apiKey.trim().length === 0) {
    throw new AuthenticationError(
      '--api-key was passed an empty value, so no key reached the CLI. ' +
        'Check that the variable holding the key is set, or omit --api-key to approve this login in the browser.',
    )
  }
  return 'api-key'
}

export async function login(options: LoginOptions): Promise<void> {
  newline()

  const method = resolveLoginMethod(options)
  const deviceMode = resolveDeviceLoginMode(options)

  const debug = debugEnabled(options)
  if (debug) {
    debugLog('login invoked with options:', {
      url: options.url,
      context: options.context,
      hasApiKey: !!options.apiKey,
    })
  }

  // If the user is already logged in AND isn't pointing at a new server /
  // context, refuse — they can `temps logout` to switch. When a new --url
  // or --context is supplied we treat the call as "add another context"
  // so the existing one stays usable via `temps context use`.
  const switchingContext = !!(options.url || options.context)
  if (!switchingContext && (await credentials.isAuthenticated())) {
    const existingEmail = await credentials.get('email')
    info(`Already logged in as ${colors.bold(existingEmail ?? 'unknown')}`)
    info('Run "temps logout" first to switch accounts, or pass --url / --context to add another.')
    return
  }

  // Headless / CI path: caller supplied a pre-minted API key.
  if (method === 'api-key' && options.apiKey) {
    await loginWithApiKey(options.apiKey, {
      url: options.url,
      context: options.context,
      debug,
    })
    return
  }

  // Interactive path: browser-based device-authorization. Credentials are
  // always entered in the web app — the CLI never prompts for a password.
  if (deviceMode === 'start') {
    await startDeviceLogin({ url: options.url, context: options.context, debug })
    return
  }
  if (deviceMode === 'resume') {
    await resumeDeviceLogin({
      url: options.url,
      context: options.context,
      waitSecs: parseResumeWaitSecs(options.waitSecs),
      debug,
    })
    return
  }
  await loginWithDevice({ url: options.url, context: options.context, debug })
}

interface DeviceSession {
  apiBaseUrl: string
  webBaseUrl: string
  start: DeviceStartResponse
  /** Absolute URL the person opens to approve. */
  verificationUrl: string
}

/**
 * Ask the server for a device code. Shared by the blocking login and by
 * `--no-wait`.
 */
async function requestDeviceSession(
  opts: { url?: string; debug: boolean },
): Promise<DeviceSession> {
  const { debug } = opts
  // `apiBaseUrl` is the `/api`-prefixed URL the auth plugin actually lives at
  // (since `temps-core` nests plugin routes under `/api`). `webBaseUrl` is the
  // frontend root, used to resolve `/cli-login` URLs the user opens in a browser.
  const apiBaseUrl = opts.url
    ? serverBaseUrl(opts.url)
    : serverBaseUrl(config.get('apiUrl'))
  const webBaseUrl = apiBaseUrl.replace(/\/api$/, '')
  if (debug) {
    debugLog(`resolved apiBaseUrl: ${apiBaseUrl}`)
    debugLog(`resolved webBaseUrl: ${webBaseUrl}`)
    debugLog(`raw url arg: ${opts.url ?? '(none, using config apiUrl)'}`)
  }

  const deviceName = (() => {
    try {
      return hostname() || 'cli'
    } catch {
      return 'cli'
    }
  })()

  const startUrl = `${apiBaseUrl}/auth/cli/device/start`
  const start = await withSpinner(
    'Requesting device authorization...',
    async () => {
      const { res, rawBody, json } = await debugFetch(
        startUrl,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ client_name: deviceName }),
        },
        debug,
      )
      if (!res.ok) {
        const problem = (json as { title?: string; detail?: string } | null) ?? null
        throw new AuthenticationError(
          problem?.detail ||
            problem?.title ||
            `Device authorization start failed at ${startUrl} (status ${res.status}). ` +
              `Response body: ${rawBody.slice(0, 200) || '(empty)'}`,
        )
      }
      if (json === null) {
        throw new AuthenticationError(
          `Server at ${startUrl} returned a non-JSON response (status ${res.status}, ` +
            `content-type ${res.headers.get('content-type') ?? 'unknown'}). ` +
            `First 200 chars: ${rawBody.slice(0, 200) || '(empty)'}. ` +
            `Re-run with --debug for the full response.`,
        )
      }
      return json as DeviceStartResponse
    },
    { successText: 'Device authorization ready' },
  )

  // `verification_uri{,_complete}` are frontend paths (e.g. `/cli-login/CODE`),
  // so they must resolve against the web root, NOT the `/api` base.
  return {
    apiBaseUrl,
    webBaseUrl,
    start,
    verificationUrl: absoluteVerificationUri(webBaseUrl, start),
  }
}

/** A URL in the form contexts store it, comparable however it was typed. */
function comparableApiBase(url: string): string {
  const base = serverBaseUrl(url)
  try {
    const parsed = new URL(base)
    return `${parsed.origin}${parsed.pathname.replace(/\/+$/, '')}`
  } catch {
    return base
  }
}

/**
 * The split sign-in refuses to save under a context name that already
 * belongs to another server. Two servers can map to one name
 * (`temps.example.com` and `temps-example.com` are both `temps-example-com`
 * in a console prompt), and replacing the person's context would point
 * every later command that names it at the wrong server.
 */
export class ContextTakenError extends CliError {
  constructor(context: string, existingUrl: string, requestedUrl: string) {
    super(
      `Context "${context}" already points to ${existingUrl}, not ${requestedUrl}. Nothing was changed. ` +
        `Sign in under another name with --context <name>, or run \`temps context remove ${context}\` first if that context is no longer used.`,
      'CONTEXT_TAKEN',
      1,
    )
    this.name = 'ContextTakenError'
  }
}

async function ensureContextIsFor(context: string, apiBaseUrl: string): Promise<void> {
  const existing = await getContext(context)
  if (existing && comparableApiBase(existing.url) !== comparableApiBase(apiBaseUrl)) {
    throw new ContextTakenError(context, serverBaseUrl(existing.url), serverBaseUrl(apiBaseUrl))
  }
}

/** Save the credentials an approved device session delivered. */
async function persistDeviceLogin(
  success: ApprovedDevicePollResponse,
  contextName: string,
  apiBaseUrl: string,
): Promise<void> {
  // We store the `/api`-prefixed URL since that's what the rest of the CLI
  // (and `normalizeApiUrl`) expects as `apiUrl`.
  await upsertContext({
    name: contextName,
    url: apiBaseUrl,
    apiKey: success.api_key,
    email: success.email,
    keyPrefix: success.key_prefix,
    expiresAt: success.expires_at ?? undefined,
  })
  config.set('apiUrl', apiBaseUrl)
  await credentials.setAll({
    apiKey: success.api_key,
    userId: success.user_id,
    email: success.email,
  })

  displayWelcome(success.email, contextName, apiBaseUrl, {
    role: success.role,
    key_prefix: success.key_prefix,
    expires_at: success.expires_at,
  })
}

/**
 * The lines `--no-wait` prints. Plain text with the link alone on its line,
 * no box and no colour: the reader is as likely to be an agent relaying the
 * link to a person as a person, and either must be able to lift it out.
 */
export function formatPendingLoginInstructions(
  pending: Pick<PendingLogin, 'verificationUrl' | 'userCode' | 'context' | 'expiresAt' | 'apiBaseUrl'>,
  now: number,
): string[] {
  const minutes = Math.max(1, Math.round((Date.parse(pending.expiresAt) - now) / 60_000))
  const webBaseUrl = pending.apiBaseUrl.replace(/\/api$/, '')
  return [
    'Approve this sign-in in a browser where you are signed in to Temps:',
    '',
    `  ${pending.verificationUrl}`,
    '',
    `Code: ${pending.userCode} (valid for ${minutes} minute${minutes === 1 ? '' : 's'})`,
    'Nothing is signed in yet. After approving, finish with:',
    '',
    `  temps login ${webBaseUrl} --context ${pending.context} --resume`,
  ]
}

/**
 * First half of the split sign-in (`--no-wait`): request a device code,
 * print the approval link, remember the code, and exit without waiting.
 */
export async function startDeviceLogin(
  opts: { url?: string; context?: string; debug?: boolean } = {},
): Promise<void> {
  const debug = debugEnabled(opts)
  const requestedBase = serverBaseUrl(opts.url ?? config.get('apiUrl'))
  const contextName = opts.context ?? defaultContextName(requestedBase)
  // Before the server mints anything: a sign-in that could only be saved by
  // replacing another server's context is not started at all.
  await ensureContextIsFor(contextName, requestedBase)
  const session = await requestDeviceSession({ url: opts.url, debug })
  const now = Date.now()
  const pending: PendingLogin = {
    context: contextName,
    apiBaseUrl: session.apiBaseUrl,
    deviceCode: session.start.device_code,
    userCode: session.start.user_code,
    verificationUrl: session.verificationUrl,
    intervalSecs: session.start.interval ?? 2,
    expiresAt: new Date(now + (session.start.expires_in ?? 900) * 1000).toISOString(),
  }
  await savePendingLogin(pending, now)
  newline()
  for (const line of formatPendingLoginInstructions(pending, now)) {
    console.log(line)
  }
}

/**
 * Thrown by `--resume` when the wait ran out before the person approved. Its
 * own exit code, so a caller can tell "not yet, run it again" from a sign-in
 * that failed.
 */
export class LoginStillPendingError extends CliError {
  constructor(pending: Pick<PendingLogin, 'verificationUrl' | 'context'>, waitedSecs: number) {
    super(
      `Still waiting for approval after ${waitedSecs}s. Nothing is signed in yet. ` +
        `Approve ${pending.verificationUrl} in the browser, then run the same command again ` +
        `(context "${pending.context}").`,
      'LOGIN_PENDING',
      LOGIN_STILL_PENDING_EXIT_CODE,
    )
    this.name = 'LoginStillPendingError'
  }
}

/**
 * Second half of the split sign-in (`--resume`): wait for the approval of
 * the sign-in `--no-wait` started for this context, then save the
 * credentials. The wait is bounded so a caller with a command timeout gets a
 * clean "not yet" (exit code 2) and can run it again; the started sign-in
 * stays on disk until it is approved, denied or expired.
 */
export async function resumeDeviceLogin(
  opts: { url?: string; context?: string; waitSecs?: number; debug?: boolean } = {},
): Promise<void> {
  const debug = debugEnabled(opts)
  const requestedBase = opts.url ? serverBaseUrl(opts.url) : undefined
  const contextName =
    opts.context ?? defaultContextName(requestedBase ?? serverBaseUrl(config.get('apiUrl')))
  const pending = await getPendingLogin(contextName)
  const restart = `temps login ${opts.url ?? '<url>'} --context ${contextName} --no-wait`
  if (!pending) {
    throw new AuthenticationError(
      `No sign-in is waiting for context "${contextName}". Start one with: ${restart}`,
    )
  }
  if (requestedBase && requestedBase !== pending.apiBaseUrl) {
    throw new AuthenticationError(
      `The sign-in waiting for context "${contextName}" was started against ${pending.apiBaseUrl}, not ${requestedBase}. ` +
        `Start a new one with: ${restart}`,
    )
  }
  await ensureContextIsFor(contextName, pending.apiBaseUrl)
  const now = Date.now()
  if (pendingLoginExpired(pending, now)) {
    await clearPendingLogin(contextName, pending.deviceCode)
    throw new AuthenticationError(
      `The sign-in started for context "${contextName}" expired before it was approved. Start a new one with: ${restart}`,
    )
  }

  const waitSecs = opts.waitSecs ?? DEFAULT_RESUME_WAIT_SECS
  const remainingSecs = Math.max(1, Math.floor((Date.parse(pending.expiresAt) - now) / 1000))
  const boundedByWait = waitSecs < remainingSecs
  const pollUrl = `${pending.apiBaseUrl}/auth/cli/device/poll`
  let success: ApprovedDevicePollResponse
  try {
    success = await withSpinner(
      `Waiting for browser approval (code ${pending.userCode})...`,
      () =>
        pollForDeviceApproval({
          pollUrl,
          intervalSecs: pending.intervalSecs,
          expiresInSecs: Math.min(waitSecs, remainingSecs),
          timeoutError: boundedByWait
            ? () => new LoginStillPendingError(pending, waitSecs)
            : undefined,
          poll: (signal) =>
            debugFetch(
              pollUrl,
              {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ device_code: pending.deviceCode }),
                signal,
              },
              debug,
            ),
        }),
      { successText: 'Browser approval received' },
    )
  } catch (err) {
    // Denied, expired or rejected by the server: this device code is dead.
    if (err instanceof DeviceLoginEndedError) {
      await clearPendingLogin(contextName, pending.deviceCode)
      throw err
    }
    if (err instanceof LoginStillPendingError) throw err
    // A dropped connection or a server error is not the end of the sign-in:
    // the person's link still works, so keep it and say how to go on.
    const reason = err instanceof Error ? err.message : String(err)
    throw new AuthenticationError(
      `${reason} The sign-in is still waiting: run the same command again to keep waiting for the same approval.`,
    )
  }

  // Only this sign-in: a newer --no-wait may have replaced it meanwhile.
  await clearPendingLogin(contextName, pending.deviceCode)
  // Checked again: another sign-in may have taken the name while this waited.
  await ensureContextIsFor(contextName, pending.apiBaseUrl)
  await persistDeviceLogin(success, contextName, pending.apiBaseUrl)
}

/**
 * Browser-based device-authorization flow. The CLI requests a `device_code`
 * + `user_code` from the server, opens the matching browser URL, and polls
 * until the user approves the device in the web UI. The user never types
 * their password into the terminal — they confirm in the regular web login
 * screen, which is critical for:
 *   - sandbox / workspace environments where prompting for a password is wrong;
 *   - SSO / magic-link accounts that have no password at all;
 *   - keeping the credential surface area in the browser, not the shell.
 */
export async function loginWithDevice(
  opts: { url?: string; context?: string; debug?: boolean } = {},
): Promise<void> {
  const debug = debugEnabled(opts)

  if (opts.url) {
    config.set('apiUrl', serverBaseUrl(opts.url))
  }

  // 1. Start a device session.
  const { apiBaseUrl, webBaseUrl, start, verificationUrl: fullUrl } =
    await requestDeviceSession({ url: opts.url, debug })

  // 2. Tell the user what to do.
  newline()
  box(
    [
      `Open this URL to authorize:`,
      `  ${colors.primary(fullUrl)}`,
      ``,
      `If your browser doesn't open automatically, paste the code:`,
      `  ${colors.bold(start.user_code)}`,
      `at ${colors.muted(`${webBaseUrl}/cli-login`)}`,
    ].join('\n'),
    `${icons.sparkles} Authorize the Temps CLI`,
  )

  // 3. Best-effort browser open. Sandboxes / SSH won't have a browser at
  // all — that's fine, the URL is right there in the box above.
  await tryOpenBrowser(fullUrl)

  // 4. Poll for approval. The server is the authority on the polling
  // interval — it returns `slow_down` if we're too eager.
  const pollUrl = `${apiBaseUrl}/auth/cli/device/poll`
  const success = await withSpinner(
    `Waiting for browser approval (code ${colors.bold(start.user_code)})...`,
    () =>
      pollForDeviceApproval({
        pollUrl,
        intervalSecs: start.interval ?? 2,
        expiresInSecs: start.expires_in ?? 900,
        poll: (signal) =>
          debugFetch(
            pollUrl,
            {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ device_code: start.device_code }),
              signal,
            },
            debug,
          ),
      }),
    { successText: 'Browser approval received' },
  )

  // 5. Persist credentials.
  const contextName = opts.context ?? defaultContextName(apiBaseUrl)
  await persistDeviceLogin(success, contextName, apiBaseUrl)
}

export interface DeviceStartResponse {
  device_code: string
  user_code: string
  verification_uri: string
  verification_uri_complete: string
  expires_in: number
  interval: number
}

type DevicePollResponse =
  | { status: 'authorization_pending' }
  | { status: 'slow_down' }
  | { status: 'access_denied' }
  | { status: 'expired_token' }
  | {
      status: 'approved'
      user_id: number
      email: string
      role: string
      api_key: string
      key_prefix: string
      expires_at?: string | null
    }

export type ApprovedDevicePollResponse = Extract<DevicePollResponse, { status: 'approved' }>

/** RFC 8628 §3.5: every `slow_down` adds 5 seconds to the polling interval. */
const SLOW_DOWN_STEP_MS = 5_000
/** Upper bound for the polling interval and for a server-requested wait. */
const MAX_POLL_DELAY_MS = 60_000

export interface PollForDeviceApprovalOptions {
  pollUrl: string
  /** Interval advertised by `/auth/cli/device/start`, in seconds. */
  intervalSecs: number
  /** Lifetime advertised by `/auth/cli/device/start`, in seconds. */
  expiresInSecs: number
  /**
   * Performs one poll request. The signal aborts when the wait runs out, so
   * a request or body read that stalls cannot hold the command past it.
   */
  poll: (signal: AbortSignal) => Promise<{ res: Response; rawBody: string; json: unknown }>
  /** Injectable for tests. */
  sleep?: (ms: number) => Promise<void>
  /** Injectable for tests. */
  now?: () => number
  /**
   * The error to throw when the wait runs out with the login still pending.
   * `--resume` passes one: its wait is shorter than the session, so running
   * out of it means "not approved yet", not "start again".
   */
  timeoutError?: () => Error
}

/**
 * The device session is over and polling it again cannot succeed: the person
 * denied it, it expired, or the server rejected the device code. Anything
 * else (a dropped connection, a server error) may pass.
 */
export class DeviceLoginEndedError extends AuthenticationError {
  constructor(message: string) {
    super(message)
    this.name = 'DeviceLoginEndedError'
  }
}

/** 4xx other than "too many requests" and "request timeout": the request itself is refused. */
function refusesDeviceCode(status: number): boolean {
  return status >= 400 && status < 500 && status !== 408 && status !== 429
}

/**
 * Parse a `Retry-After` header (delay-seconds or HTTP-date) into milliseconds
 * from `now`. Returns `null` when absent or unparsable.
 */
export function retryAfterMs(header: string | null, now: number): number | null {
  if (!header) return null
  const trimmed = header.trim()
  if (/^\d+$/.test(trimmed)) return Number(trimmed) * 1000
  const date = Date.parse(trimmed)
  if (Number.isNaN(date)) return null
  return Math.max(0, date - now)
}

/**
 * Poll the device-authorization endpoint until the login is approved,
 * denied or expired (RFC 8628 §3.4-3.5).
 *
 * `slow_down` permanently lengthens the interval by 5 seconds. HTTP 429 is
 * treated the same way (a rate limiter in front of the endpoint is asking
 * for the same thing), and a `Retry-After` on it is honoured, so a
 * throttled poll never aborts a login the user is still approving. Other
 * error statuses fail fast with the server's problem detail.
 */
export async function pollForDeviceApproval(
  opts: PollForDeviceApprovalOptions,
): Promise<ApprovedDevicePollResponse> {
  const sleepFn = opts.sleep ?? sleep
  const now = opts.now ?? Date.now
  const deadline = now() + opts.expiresInSecs * 1000
  let intervalMs = Math.min(Math.max(500, opts.intervalSecs * 1000), MAX_POLL_DELAY_MS)
  let nextDelayMs = intervalMs
  let throttledLastPoll = false

  for (;;) {
    const remainingMs = deadline - now()
    if (remainingMs <= 0) break
    // Never sleep past the session's lifetime.
    await sleepFn(Math.min(nextDelayMs, remainingMs))
    const leftMs = deadline - now()
    if (leftMs <= 0) break
    // The deadline bounds the request too, not only the loop.
    const abort = new AbortController()
    const timer = setTimeout(() => abort.abort(new Error('poll deadline reached')), leftMs)
    let reply: { res: Response; rawBody: string; json: unknown }
    try {
      reply = await opts.poll(abort.signal)
    } catch (err) {
      if (abort.signal.aborted) break
      throw err
    } finally {
      clearTimeout(timer)
    }
    const { res, rawBody, json } = reply

    if (res.status === 429) {
      throttledLastPoll = true
      intervalMs = Math.min(intervalMs + SLOW_DOWN_STEP_MS, MAX_POLL_DELAY_MS)
      const retryAfter = retryAfterMs(res.headers.get('retry-after'), now())
      nextDelayMs = Math.min(Math.max(intervalMs, retryAfter ?? 0), MAX_POLL_DELAY_MS)
      continue
    }
    throttledLastPoll = false

    if (!res.ok) {
      const problem = (json as { title?: string; detail?: string } | null) ?? null
      const Failure = refusesDeviceCode(res.status) ? DeviceLoginEndedError : AuthenticationError
      throw new Failure(
        problem?.detail ||
          problem?.title ||
          `Polling failed at ${opts.pollUrl} (status ${res.status}). ` +
            `Response body: ${rawBody.slice(0, 200) || '(empty)'}`,
      )
    }
    if (json === null) {
      throw new AuthenticationError(
        `Server at ${opts.pollUrl} returned a non-JSON response (status ${res.status}). ` +
          `First 200 chars: ${rawBody.slice(0, 200) || '(empty)'}`,
      )
    }
    const body = json as DevicePollResponse
    switch (body.status) {
      case 'authorization_pending':
        nextDelayMs = intervalMs
        continue
      case 'slow_down':
        intervalMs = Math.min(intervalMs + SLOW_DOWN_STEP_MS, MAX_POLL_DELAY_MS)
        nextDelayMs = intervalMs
        continue
      case 'access_denied':
        throw new DeviceLoginEndedError('Authorization denied in the browser.')
      case 'expired_token':
        throw new DeviceLoginEndedError(
          'Authorization code expired before approval. Run `temps login` again.',
        )
      case 'approved':
        return body
    }
  }
  if (opts.timeoutError) throw opts.timeoutError()
  throw new AuthenticationError(
    throttledLastPoll
      ? `Timed out waiting for browser approval: the server kept rate limiting ${opts.pollUrl} (HTTP 429). ` +
          'Run `temps login` again, or ask the server operator to check its rate limits.'
      : 'Timed out waiting for browser approval. Run `temps login` again.',
  )
}

/** Resolve a possibly-relative `verification_uri_complete` against the base URL. */
export function absoluteVerificationUri(
  baseUrl: string,
  start: DeviceStartResponse,
): string {
  if (/^https?:\/\//i.test(start.verification_uri_complete)) {
    return start.verification_uri_complete
  }
  const path = start.verification_uri_complete.startsWith('/')
    ? start.verification_uri_complete
    : `/${start.verification_uri_complete}`
  return `${baseUrl.replace(/\/+$/, '')}${path}`
}

/**
 * Best-effort browser launcher. Detached so the CLI doesn't block on it.
 * Failures are silently ignored — we already printed the URL.
 */
async function tryOpenBrowser(url: string): Promise<void> {
  // Common headless signals: respect them rather than spawning a process
  // that will print noise to stderr.
  if (process.env.CI || process.env.TEMPS_NO_BROWSER || !process.stdout.isTTY) {
    return
  }

  const plat = platform()
  let cmd: string
  let args: string[]
  if (plat === 'darwin') {
    cmd = 'open'
    args = [url]
  } else if (plat === 'win32') {
    cmd = 'cmd'
    args = ['/c', 'start', '""', url]
  } else {
    cmd = 'xdg-open'
    args = [url]
  }

  try {
    const child = spawn(cmd, args, {
      stdio: 'ignore',
      detached: true,
    })
    child.on('error', () => {
      // No browser, no problem — URL is already printed.
    })
    child.unref()
  } catch {
    // No-op.
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export async function loginWithApiKey(
  apiKey?: string,
  opts: { url?: string; context?: string; debug?: boolean } = {},
): Promise<void> {
  const debug = debugEnabled(opts)
  const key = apiKey ?? (await promptPassword({
    message: 'API Key',
    validate: (value) => {
      if (!value || value.trim().length === 0) {
        return 'API key is required'
      }
      return true
    },
  }))

  // Resolve the target server BEFORE validating the key. Without this, an
  // API-key login ignores the URL the caller passed (positional arg or
  // --url) and validates against whatever `config.get('apiUrl')` happens to
  // return — the active context, TEMPS_API_URL, or the localhost default.
  // On a machine with a stale/absent context that means we'd validate the
  // key against the wrong server, get "Invalid API key", and wipe creds.
  const baseUrl = opts.url
    ? serverBaseUrl(opts.url)
    : serverBaseUrl(config.get('apiUrl'))
  // The URL is persisted only after the key has been validated against it
  // (below): saving it first would leave a URL that is not a Temps server as
  // the target of every later command when validation fails.
  if (debug) {
    debugLog(`api-key login resolved apiUrl: ${baseUrl}`)
    debugLog(`raw url arg: ${opts.url ?? '(none, using config apiUrl)'}`)
  }

  // Validate the *supplied* key against the named server. We pass `key` as an
  // explicit interceptor override (rather than writing it to the credential
  // store first) so:
  //   - `getApiKey()`'s priority order (env > active context > secrets) cannot
  //     shadow it — otherwise an active context's key gets validated instead,
  //     and we'd report "Invalid API key" for a key we never sent;
  //   - a failed validation leaves no half-written credentials behind.
  await setupClient(baseUrl, key)

  try {
    const result = await withSpinner(
      'Validating API key...',
      async () => {
        const { data, error } = await getCurrentUser({ client })
        if (error) {
          // A reply that is not a Temps API answer (wrong URL, unreachable
          // host) is not an invalid key; say what actually went wrong.
          // A 401/403 is still the server rejecting the key, even when the
          // proxy in front of it sent no body.
          const rejectedKey =
            !(error instanceof ApiResponseError) ||
            error.status === 401 ||
            error.status === 403
          throw new AuthenticationError(
            rejectedKey ? 'Invalid API key' : (error as ApiResponseError).message
          )
        }
        // A 2xx without a user object is not a Temps server vouching for the
        // key, so nothing may be stored for it.
        if (!isUserObject(data)) {
          throw new AuthenticationError(
            `The server at ${baseUrl} answered but did not return a Temps user. Check the URL.`
          )
        }
        return data
      },
      { successText: 'API key validated' }
    )

    await credentials.setAll({
      apiKey: key,
      userId: result.id,
      email: result.email ?? undefined,
    })

    // Mirror the API-key login into the multi-context store so subsequent
    // commands resolve credentials uniformly. We don't know the key prefix
    // or expiry from a manually-pasted key, so those fields stay empty.
    const ctxName = opts.context ?? defaultContextName(baseUrl)
    config.set('apiUrl', baseUrl)
    await upsertContext({
      name: ctxName,
      url: baseUrl,
      apiKey: key,
      email: result.email ?? '',
      keyPrefix: key.slice(0, 8),
    })

    displayWelcome(result.email, ctxName, baseUrl)
  } catch (err) {
    // Validation failed. We deliberately did NOT pre-write the key, so there's
    // nothing of ours to roll back — and we must NOT `credentials.clear()`,
    // which would wipe a perfectly good key from a previously-authenticated
    // context. Surface the server's reason when we have one.
    if (debug && err instanceof Error) {
      debugLog(`api-key validation failed: ${err.message}`)
    }
    throw new AuthenticationError(
      err instanceof AuthenticationError
        ? err.message
        : `Invalid API key (validated against ${baseUrl})`,
    )
  } finally {
    // Drop the explicit pin so subsequent commands fall back to normal
    // priority-ordered credential resolution.
    await setupClient(baseUrl)
  }
}

export function displayWelcome(
  email?: string | null,
  contextName?: string,
  baseUrl?: string,
  meta?: { role?: string; key_prefix?: string; expires_at?: string | null },
): void {
  newline()
  const url = baseUrl ?? config.get('apiUrl')
  const role = meta?.role ? colors.muted(`(${meta.role})`) : ''
  const expires = meta?.expires_at
    ? `\nExpires: ${colors.muted(new Date(meta.expires_at).toISOString().split('T')[0] ?? meta.expires_at)}`
    : ''
  const ctxLine = contextName ? `\nContext: ${colors.bold(contextName)}` : ''
  const keyLine = meta?.key_prefix
    ? `\nKey: ${colors.muted(meta.key_prefix + '…')}`
    : ''
  const lines = [
    email ? `Logged in as ${colors.bold(email)} ${role}`.trim() : null,
    `Server: ${colors.muted(url)}${ctxLine}${keyLine}${expires}`,
    `Credentials stored in: ${colors.muted(credentials.path)}`,
  ]
    .filter(Boolean)
    .join('\n')
  box(lines, `${icons.sparkles} Welcome to Temps!`)
}
