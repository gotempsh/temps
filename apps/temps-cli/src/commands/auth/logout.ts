import { credentials, config } from '../../config/store.js'
import {
  getActiveContext,
  getContext,
  removeContext,
} from '../../config/contexts.js'
import { success, info, warning, newline, colors } from '../../ui/output.js'

interface LogoutOptions {
  context?: string
  localOnly?: boolean
}

export async function logout(options: LogoutOptions = {}): Promise<void> {
  newline()

  // Pick the context to log out of: explicit --context, else the active one,
  // else fall back to the legacy single-instance store.
  const ctx = options.context
    ? await getContext(options.context)
    : await getActiveContext()

  if (!ctx && !(await credentials.isAuthenticated())) {
    info('Not currently logged in')
    return
  }

  // Server-side revocation. Best-effort: a failed revoke (network blip,
  // server restart, key already gone) shouldn't trap the user with stale
  // local creds. Skip entirely with --local-only.
  if (!options.localOnly && ctx?.apiKey) {
    try {
      const url = ctx.url.replace(/\/+$/, '').replace(/\/api$/, '')
      const response = await fetch(`${url}/auth/cli/logout`, {
        method: 'POST',
        headers: { Authorization: `Bearer ${ctx.apiKey}` },
      })
      if (!response.ok && response.status !== 401) {
        warning(`Server-side revoke returned ${response.status}; continuing with local cleanup.`)
      }
    } catch (err) {
      warning(
        `Could not reach server to revoke key: ${err instanceof Error ? err.message : String(err)}`,
      )
      info('Local credentials will still be removed.')
    }
  }

  // Local cleanup.
  if (ctx) {
    await removeContext(ctx.name)
  }
  // Always clear the legacy single-instance secrets so a stale token
  // doesn't accidentally win the resolution chain on the next command.
  await credentials.clear()

  if (ctx) {
    success(
      `Logged out of context ${colors.bold(ctx.name)}${ctx.email ? ` (${ctx.email})` : ''}`,
    )
  } else {
    const email = await credentials.get('email')
    success(`Logged out${email ? ` from ${email}` : ''}`)
  }

  // If there's still a remaining active context, surface it so the user
  // knows they're not "fully" logged out.
  const remaining = await getActiveContext()
  if (remaining) {
    info(
      `Active context is now ${colors.bold(remaining.name)} (${colors.muted(remaining.url)}). Use \`temps context list\` to manage.`,
    )
  } else {
    info(`Run \`temps login <url>\` to authenticate against another server.`)
  }

  // Reset the legacy apiUrl back to default so subsequent commands don't
  // silently retain the old URL when the user logs into a different server.
  // (Only do this if no contexts remain — otherwise the active context
  // already drives `config.get('apiUrl')`.)
  if (!remaining) {
    config.set('apiUrl', 'http://localhost:3000')
  }
}
