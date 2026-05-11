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
}

/**
 * Strip the "/api" suffix that `normalizeApiUrl` appends, since the
 * `/auth/cli/device/*` endpoints sit at the server root, not under `/api`.
 * Also tolerates the user passing the bare host with or without scheme.
 */
function serverBaseUrl(rawApiUrl: string): string {
  return rawApiUrl.replace(/\/+$/, '').replace(/\/api$/, '')
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

  // Headless / CI path: caller supplied a pre-minted API key.
  if (options.apiKey) {
    await loginWithApiKey(options.apiKey)
    return
  }

  // Interactive path: browser-based device-authorization. Credentials are
  // always entered in the web app — the CLI never prompts for a password.
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

  // 5. Persist credentials.
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
  } catch {
    await credentials.clear()
    throw new AuthenticationError('Invalid API key')
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
