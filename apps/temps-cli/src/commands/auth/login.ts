import { credentials, config } from '../../config/store.js'
import { upsertContext, defaultContextName } from '../../config/contexts.js'
import { promptPassword, promptText, promptSelect, promptEmail } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { info, icons, colors, newline, box, warning } from '../../ui/output.js'
import { setupClient, client, normalizeApiUrl } from '../../lib/api-client.js'
import { getCurrentUser } from '../../api/sdk.gen.js'
import { AuthenticationError } from '../../utils/errors.js'
import { hostname } from 'node:os'

interface LoginOptions {
  apiKey?: string
  email?: string
  magic?: string
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

  if (options.magic) {
    await loginWithMagicLink(typeof options.magic === 'string' ? options.magic : undefined)
    return
  }

  if (options.email) {
    await loginWithEmail(typeof options.email === 'string' ? options.email : undefined, {
      url: options.url,
      context: options.context,
      mfa: options.mfa,
    })
    return
  }

  // If no specific method, check if --api-key or prompt for method
  if (options.apiKey) {
    await loginWithApiKey(options.apiKey)
    return
  }

  // Default to email + password — that's what `temps login <url>` should
  // do. The interactive picker is still available if the user wants
  // magic link or pasted API key.
  if (options.url || options.context) {
    await loginWithEmail(undefined, {
      url: options.url,
      context: options.context,
      mfa: options.mfa,
    })
    return
  }

  // Interactive: ask which method
  const method = await promptSelect({
    message: 'How would you like to log in?',
    choices: [
      { name: 'Email & Password', value: 'email', description: 'Log in with email and password' },
      { name: 'API Key', value: 'api-key', description: 'Paste an API key from the dashboard' },
      { name: 'Magic Link', value: 'magic', description: 'Receive a login link via email' },
    ],
  })

  switch (method) {
    case 'api-key':
      await loginWithApiKey()
      break
    case 'email':
      await loginWithEmail(undefined, {
        url: options.url,
        context: options.context,
        mfa: options.mfa,
      })
      break
    case 'magic':
      await loginWithMagicLink()
      break
  }
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
