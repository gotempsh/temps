import { credentials, config } from '../../config/store.js'
import { upsertContext, defaultContextName } from '../../config/contexts.js'
import { promptPassword, promptText, promptEmail } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, icons, colors, newline, box, warning } from '../../ui/output.js'
import { setupClient, client, normalizeApiUrl } from '../../lib/api-client.js'
import { getCurrentUser } from '../../api/sdk.gen.js'
import { AuthenticationError } from '../../utils/errors.js'
import { hostname, platform } from 'node:os'
import { spawn } from 'node:child_process'

interface LoginOptions {
  apiKey?: string
  email?: string
  magic?: string
  /** Force the legacy password-prompt flow (was the default pre-device-flow). */
  password?: boolean
  /** Force the device-authorization (browser) flow. Default for interactive sessions. */
  device?: boolean
  /** Optional friendly name for the saved context (defaults to URL host). */
  context?: string
  /** Override the server URL for this login (otherwise uses config / active context). */
  url?: string
  /** Pre-supplied 6-digit MFA code for non-interactive scripts. */
  mfa?: string
}

/**
 * Strip the "/api" suffix that `normalizeApiUrl` appends, since the
 * `/auth/cli/login` endpoint sits at the server root, not under `/api`.
 * Also tolerates the user passing the bare host with or without scheme.
 */
function serverBaseUrl(rawApiUrl: string): string {
  return rawApiUrl.replace(/\/+$/, '').replace(/\/api$/, '')
}

interface CliLoginSuccess {
  user_id: number
  email: string
  role: string
  api_key: string
  key_prefix: string
  expires_at?: string | null
}

interface CliLoginMfaRequired {
  mfa_required: boolean
  mfa_session_token: string
}

type CliLoginResponse = CliLoginSuccess | CliLoginMfaRequired

function isMfaChallenge(r: CliLoginResponse): r is CliLoginMfaRequired {
  return (r as CliLoginMfaRequired).mfa_required === true
}

export async function login(options: LoginOptions): Promise<void> {
  newline()

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

  // Explicit method flags take precedence over the default.
  if (options.magic) {
    await loginWithMagicLink(typeof options.magic === 'string' ? options.magic : undefined)
    return
  }
  if (options.apiKey) {
    await loginWithApiKey(options.apiKey)
    return
  }
  if (options.email || options.password) {
    await loginWithEmail(typeof options.email === 'string' ? options.email : undefined, {
      url: options.url,
      context: options.context,
      mfa: options.mfa,
    })
    return
  }
  if (options.device) {
    await loginWithDevice({ url: options.url, context: options.context })
    return
  }

  // Default flow: browser-based device-authorization. Works in workspaces /
  // sandboxes / SSO accounts where the legacy email+password prompt would
  // be wrong (no password, or no terminal you'd want to type one into).
  // The legacy methods stay opt-in via --email / --magic / --api-key.
  await loginWithDevice({ url: options.url, context: options.context })
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
  opts: { url?: string; context?: string } = {},
): Promise<void> {
  const baseUrl = opts.url
    ? serverBaseUrl(opts.url)
    : serverBaseUrl(config.get('apiUrl'))
  if (opts.url) {
    config.set('apiUrl', baseUrl)
  }

  const deviceName = (() => {
    try {
      return hostname() || 'cli'
    } catch {
      return 'cli'
    }
  })()

  // 1. Start a device session.
  const start = await withSpinner(
    'Requesting device authorization...',
    async () => {
      const res = await fetch(`${baseUrl}/auth/cli/device/start`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ client_name: deviceName }),
      })
      if (!res.ok) {
        const problem = await safeJson<{ title?: string; detail?: string }>(res)
        throw new AuthenticationError(
          problem?.detail ||
            problem?.title ||
            `Device authorization start failed (status ${res.status})`,
        )
      }
      return (await res.json()) as DeviceStartResponse
    },
    { successText: 'Device authorization ready' },
  )

  const fullUrl = absoluteVerificationUri(baseUrl, start)

  // 2. Tell the user what to do.
  newline()
  box(
    [
      `Open this URL to authorize:`,
      `  ${colors.primary(fullUrl)}`,
      ``,
      `If your browser doesn't open automatically, paste the code:`,
      `  ${colors.bold(start.user_code)}`,
      `at ${colors.muted(`${baseUrl}/cli-login`)}`,
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
      // Loop until the server reaches a terminal state or we time out.
      while (Date.now() < deadline) {
        await sleep(pollDelay)
        const res = await fetch(`${baseUrl}/auth/cli/device/poll`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ device_code: start.device_code }),
        })
        if (!res.ok) {
          const problem = await safeJson<{ title?: string; detail?: string }>(res)
          throw new AuthenticationError(
            problem?.detail ||
              problem?.title ||
              `Polling failed (status ${res.status})`,
          )
        }
        const body = (await res.json()) as DevicePollResponse
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

  // 5. Persist credentials. Same machinery as the email-password flow.
  const contextName = opts.context ?? defaultContextName(baseUrl)
  await upsertContext({
    name: contextName,
    url: baseUrl,
    apiKey: success.api_key,
    email: success.email,
    keyPrefix: success.key_prefix,
    expiresAt: success.expires_at ?? undefined,
  })
  config.set('apiUrl', baseUrl)
  await credentials.setAll({
    apiKey: success.api_key,
    userId: success.user_id,
    email: success.email,
  })

  displayWelcome(success.email, contextName, baseUrl, {
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

async function safeJson<T>(res: Response): Promise<T | null> {
  try {
    return (await res.json()) as T
  } catch {
    return null
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export async function loginWithApiKey(apiKey?: string): Promise<void> {
  const key = apiKey ?? (await promptPassword({
    message: 'API Key',
    validate: (value) => {
      if (!value || value.trim().length === 0) {
        return 'API key is required'
      }
      return true
    },
  }))

  // Temporarily set the API key to validate it
  await credentials.set('apiKey', key)
  await setupClient()

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
    const baseUrl = serverBaseUrl(config.get('apiUrl'))
    const ctxName = defaultContextName(baseUrl)
    await upsertContext({
      name: ctxName,
      url: baseUrl,
      apiKey: key,
      email: result.email ?? '',
      keyPrefix: key.slice(0, 8),
    })

    displayWelcome(result.email, ctxName, baseUrl)
  } catch (err) {
    await credentials.clear()
    throw new AuthenticationError('Invalid API key')
  }
}

export async function loginWithEmail(
  emailArg?: string,
  opts: { url?: string; context?: string; mfa?: string } = {},
): Promise<void> {
  // Resolve the server URL: --url > --api-url env > current config / active context
  const baseUrl = opts.url
    ? serverBaseUrl(opts.url)
    : serverBaseUrl(config.get('apiUrl'))

  // If the user is overriding the URL on the command line, persist it for
  // the rest of this login attempt so the API client picks it up too. The
  // active context (created at the end) becomes the durable source.
  if (opts.url) {
    config.set('apiUrl', baseUrl)
  }

  const email = emailArg ?? (await promptEmail('Email'))

  const password = await promptPassword({
    message: 'Password',
    validate: (value) => {
      if (!value || value.trim().length === 0) {
        return 'Password is required'
      }
      return true
    },
  })

  const deviceName = (() => {
    try {
      return hostname() || 'cli'
    } catch {
      return 'cli'
    }
  })()

  // First call: send password (and the supplied --mfa code, if any). The
  // server either returns a key directly or asks for MFA.
  const firstResponse = await withSpinner('Logging in...', async () => {
    return cliLoginRequest(baseUrl, {
      email,
      password,
      mfa_code: opts.mfa,
      device_name: deviceName,
    })
  }, { successText: 'Authenticated' })

  let success: CliLoginSuccess
  if (isMfaChallenge(firstResponse)) {
    const mfaCode = opts.mfa ?? await promptText({
      message: 'MFA Code',
      required: true,
      validate: (value) => /^\d{6}$/.test(value) || 'Enter a 6-digit code',
    })

    success = await withSpinner('Verifying MFA...', async () => {
      const r = await cliLoginRequest(baseUrl, {
        email,
        password,
        mfa_code: mfaCode,
        mfa_session_token: firstResponse.mfa_session_token,
        device_name: deviceName,
      })
      if (isMfaChallenge(r)) {
        throw new AuthenticationError('Server requested MFA again after we supplied a code')
      }
      return r
    }, { successText: 'MFA verified' })
  } else {
    success = firstResponse
  }

  // Persist into the multi-context store. This is the durable source of
  // truth — `config.get('apiUrl')` and `credentials.getApiKey()` read the
  // active context first, so every other command immediately sees the
  // new credentials with no extra plumbing.
  const contextName = opts.context ?? defaultContextName(baseUrl)
  await upsertContext({
    name: contextName,
    url: baseUrl,
    apiKey: success.api_key,
    email: success.email || email,
    keyPrefix: success.key_prefix,
    expiresAt: success.expires_at ?? undefined,
  })

  // Mirror into the legacy single-instance store so commands that haven't
  // migrated to context-aware lookups (or older script invocations) still
  // see the new credentials.
  config.set('apiUrl', baseUrl)
  await credentials.setAll({
    apiKey: success.api_key,
    userId: success.user_id,
    email: success.email || email,
  })

  displayWelcome(success.email || email, contextName, baseUrl, success)
}

/**
 * One round-trip to `POST /auth/cli/login`. Returns either a successful
 * key mint or an MFA challenge — the caller decides what to do next.
 */
async function cliLoginRequest(
  baseUrl: string,
  body: {
    email: string
    password: string
    mfa_code?: string
    mfa_session_token?: string
    device_name?: string
  },
): Promise<CliLoginResponse> {
  const url = `${baseUrl}/auth/cli/login`
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!response.ok) {
    // Try to surface the server's RFC 7807 problem detail when present.
    let detail = ''
    try {
      const problem = (await response.json()) as { title?: string; detail?: string }
      detail = problem.detail || problem.title || ''
    } catch {
      // Non-JSON or empty body — fall through.
    }
    if (response.status === 401) {
      throw new AuthenticationError(detail || 'Invalid email or password')
    }
    if (response.status === 403) {
      throw new AuthenticationError(detail || 'Access denied')
    }
    throw new AuthenticationError(
      detail
        ? `Login failed (${response.status}): ${detail}`
        : `Login failed (status ${response.status})`,
    )
  }
  return (await response.json()) as CliLoginResponse
}

export async function loginWithMagicLink(emailArg?: string): Promise<void> {
  const email = emailArg ?? await promptEmail('Email')
  const apiUrl = normalizeApiUrl(config.get('apiUrl'))

  await withSpinner('Sending magic link...', async () => {
    const response = await fetch(`${apiUrl}/auth/magic-link/request`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email }),
    })

    if (!response.ok && response.status !== 200) {
      if (response.status === 503) {
        throw new Error('Email service is not configured on this server')
      }
      throw new Error('Failed to send magic link')
    }
  }, { successText: 'Magic link sent' })

  newline()
  info('Check your email for a magic link.')
  info('After clicking the link, paste the token from the URL below.')
  newline()

  const tokenInput = await promptText({
    message: 'Magic link token (from URL or email)',
    required: true,
  })

  // Extract token from URL if user pastes full URL
  const token = extractTokenFromInput(tokenInput)

  // Verify the magic link token using raw fetch to capture session cookie
  const verifyResult = await withSpinner('Verifying token...', async () => {
    const response = await fetch(`${apiUrl}/auth/magic-link/verify?token=${encodeURIComponent(token)}`, {
      method: 'GET',
      headers: { 'Content-Type': 'application/json' },
    })

    if (!response.ok) {
      if (response.status === 400) {
        throw new AuthenticationError('Invalid or expired token')
      }
      throw new AuthenticationError('Verification failed')
    }

    const data = await response.json() as { success: boolean; message: string }
    const setCookie = response.headers.get('set-cookie')

    return { data, setCookie }
  }, { successText: 'Token verified' })

  // Create API token from session
  const sessionCookie = extractSessionCookie(verifyResult.setCookie)

  if (!sessionCookie) {
    warning('Could not extract session. Please create an API key from the dashboard.')
    info(`Dashboard: ${colors.primary(`${apiUrl}/dashboard/settings/api-keys`)}`)
    newline()
    info('Then run: temps login --api-key <your-key>')
    return
  }

  const apiKey = await withSpinner('Creating API token...', async () => {
    const response = await fetch(`${apiUrl}/api/tokens`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'Cookie': sessionCookie,
      },
      body: JSON.stringify({
        name: `temps-cli-${new Date().toISOString().split('T')[0]}`,
        permissions: ['*'],
      }),
    })

    if (!response.ok) {
      throw new Error('Could not create API token from session')
    }

    const tokenData = await response.json() as { token?: string; api_key?: string }
    return tokenData.token ?? tokenData.api_key
  }, { successText: 'API token created' })

  if (!apiKey) {
    warning('Could not create API token. Please generate one from the dashboard.')
    return
  }

  await credentials.set('apiKey', apiKey)
  await setupClient()

  const user = await withSpinner('Validating...', async () => {
    const { data, error } = await getCurrentUser({ client })
    if (error || !data) throw new AuthenticationError('Token validation failed')
    return data
  }, { successText: 'Validated' })

  await credentials.setAll({
    apiKey,
    userId: user.id,
    email: user.email ?? undefined,
  })

  const baseUrl = serverBaseUrl(config.get('apiUrl'))
  const ctxName = defaultContextName(baseUrl)
  await upsertContext({
    name: ctxName,
    url: baseUrl,
    apiKey,
    email: user.email ?? '',
    keyPrefix: apiKey.slice(0, 8),
  })

  displayWelcome(user.email, ctxName, baseUrl)
}

// ── Helpers ──

function extractCookieValue(setCookie: string | null, name: string): string | null {
  if (!setCookie) return null

  // set-cookie can have multiple cookies separated by commas (or multiple headers)
  const cookies = setCookie.split(/,(?=\s*\w+=)/)
  for (const cookie of cookies) {
    const match = cookie.match(new RegExp(`${name}=([^;]+)`))
    if (match) return `${name}=${match[1]}`
  }
  return null
}

function extractSessionCookie(setCookie: string | null): string | null {
  if (!setCookie) return null

  // Try common session cookie names
  const cookieNames = ['session_token', 'session', 'id', 'sid']
  for (const name of cookieNames) {
    const cookie = extractCookieValue(setCookie, name)
    if (cookie) return cookie
  }

  // Fallback: return the full set-cookie for single cookie
  const match = setCookie.match(/^([^=]+=[^;]+)/)
  if (match?.[1]) return match[1]

  return null
}

function extractTokenFromInput(input: string): string {
  const trimmed = input.trim()

  // If it looks like a URL, extract the token parameter
  try {
    const url = new URL(trimmed)
    const token = url.searchParams.get('token')
    if (token) return token
  } catch {
    // Not a URL, treat as raw token
  }

  return trimmed
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
