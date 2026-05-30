import { credentials, config } from '../../config/store.js'
import { upsertContext, defaultContextName } from '../../config/contexts.js'
import { promptPassword } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, icons, colors, newline, box } from '../../ui/output.js'
import { setupClient, client } from '../../lib/api-client.js'
import { getCurrentUser } from '../../api/sdk.gen.js'
import { AuthenticationError } from '../../utils/errors.js'
import { hostname, platform } from 'node:os'
import { spawn } from 'node:child_process'

interface LoginOptions {
  /** Pre-minted API key from the dashboard's Settings → API Keys page. Use this for headless / CI. */
  apiKey?: string
  /** Optional friendly name for the saved context (defaults to URL host). */
  context?: string
  /** Override the server URL for this login (otherwise uses config / active context). */
  url?: string
  /** Emit verbose request/response logging for diagnosing connection issues. */
  debug?: boolean
}

/**
 * Whether verbose request/response logging is enabled for this invocation.
 * Activated by `--debug` on the command or `TEMPS_DEBUG=1` in the environment.
 */
function debugEnabled(opts: { debug?: boolean } = {}): boolean {
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
    res = await fetch(url, init)
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err)
    if (debug) debugLog(`   fetch failed: ${reason}`)
    throw new AuthenticationError(
      `Unable to connect to ${url}: ${reason}. Is the server reachable from this machine?`,
    )
  }
  const rawBody = await res.text()
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
function serverBaseUrl(rawApiUrl: string): string {
  const trimmed = rawApiUrl.replace(/\/+$/, '')
  return /\/api$/.test(trimmed) ? trimmed : `${trimmed}/api`
}

export async function login(options: LoginOptions): Promise<void> {
  newline()

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
  if (options.apiKey) {
    await loginWithApiKey(options.apiKey, {
      url: options.url,
      context: options.context,
      debug,
    })
    return
  }

  // Interactive path: browser-based device-authorization. Credentials are
  // always entered in the web app — the CLI never prompts for a password.
  await loginWithDevice({ url: options.url, context: options.context, debug })
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
  // `apiBaseUrl` is the `/api`-prefixed URL the auth plugin actually lives at
  // (since `temps-core` nests plugin routes under `/api`). `webBaseUrl` is the
  // frontend root, used to resolve `/cli-login` URLs the user opens in a browser.
  const apiBaseUrl = opts.url
    ? serverBaseUrl(opts.url)
    : serverBaseUrl(config.get('apiUrl'))
  const webBaseUrl = apiBaseUrl.replace(/\/api$/, '')
  if (opts.url) {
    config.set('apiUrl', apiBaseUrl)
  }
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

  // 1. Start a device session.
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
  const fullUrl = absoluteVerificationUri(webBaseUrl, start)

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
  const intervalMs = Math.max(500, (start.interval ?? 2) * 1000)
  const deadline = Date.now() + (start.expires_in ?? 900) * 1000

  const success = await withSpinner(
    `Waiting for browser approval (code ${colors.bold(start.user_code)})...`,
    async () => {
      let pollDelay = intervalMs
      const pollUrl = `${apiBaseUrl}/auth/cli/device/poll`
      // Loop until the server reaches a terminal state or we time out.
      while (Date.now() < deadline) {
        await sleep(pollDelay)
        const { res, rawBody, json } = await debugFetch(
          pollUrl,
          {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ device_code: start.device_code }),
          },
          debug,
        )
        if (!res.ok) {
          const problem = (json as { title?: string; detail?: string } | null) ?? null
          throw new AuthenticationError(
            problem?.detail ||
              problem?.title ||
              `Polling failed at ${pollUrl} (status ${res.status}). ` +
                `Response body: ${rawBody.slice(0, 200) || '(empty)'}`,
          )
        }
        if (json === null) {
          throw new AuthenticationError(
            `Server at ${pollUrl} returned a non-JSON response (status ${res.status}). ` +
              `First 200 chars: ${rawBody.slice(0, 200) || '(empty)'}`,
          )
        }
        const body = json as DevicePollResponse
        switch (body.status) {
          case 'authorization_pending':
            pollDelay = intervalMs
            continue
          case 'slow_down':
            // Server-suggested backoff. Double up to a cap.
            pollDelay = Math.min(pollDelay * 2, 10_000)
            continue
          case 'access_denied':
            throw new AuthenticationError(
              'Authorization denied in the browser.',
            )
          case 'expired_token':
            throw new AuthenticationError(
              'Authorization code expired before approval. Run `temps login` again.',
            )
          case 'approved':
            return body
        }
      }
      throw new AuthenticationError(
        'Timed out waiting for browser approval. Run `temps login` again.',
      )
    },
    { successText: 'Browser approval received' },
  )

  // 5. Persist credentials. We store the `/api`-prefixed URL since that's
  // what the rest of the CLI (and `normalizeApiUrl`) expects as `apiUrl`.
  const contextName = opts.context ?? defaultContextName(apiBaseUrl)
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

interface DeviceStartResponse {
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

/** Resolve a possibly-relative `verification_uri_complete` against the base URL. */
function absoluteVerificationUri(
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
  if (opts.url) {
    config.set('apiUrl', baseUrl)
  }
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
          throw new AuthenticationError('Invalid API key')
        }
        return data
      },
      { successText: 'API key validated' }
    )
    if (!result) {
      throw new AuthenticationError('Invalid API key')
    }

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
