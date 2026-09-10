// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { createApiKey, getSettings, updateSettings } from '../../api/sdk.gen.js'
import type { AppSettings } from '../../api/types.gen.js'
import { loginWithDevice } from '../auth/login.js'
import { getActiveContext, getContext, listContexts } from '../../config/contexts.js'
import { credentials, getApiUrl, requireAuth } from '../../config/store.js'
import { client, getErrorMessage, getWebUrl, setupClient } from '../../lib/api-client.js'
import { header, icons, info, keyValue, newline, success, warning } from '../../ui/output.js'
import { promptCheckbox, promptConfirm, promptSearch, promptSelect, promptText } from '../../ui/prompts.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { CLIENT_ADAPTERS, getClientAdapter, listClientIds } from './clients/index.js'
import { TOKEN_ENV_VAR as CODEX_TOKEN_ENV_VAR } from './clients/codex.js'
import { buildMcpUrl, isValidGroupKey, parseMcpUrl, TOOL_GROUPS } from './groups.js'
import { probeMcpEndpoint } from './probe.js'

/**
 * The scheme+host+port a URL resolves to, for comparing whether two URLs
 * point at the same Temps instance. Deliberately includes the scheme, not
 * just host:port -- a saved https:// context's bearer key must never be
 * treated as valid for an http:// target sharing the same hostname, which
 * would bind (and potentially transmit) that credential over plaintext.
 * Returns null for an unparsable URL so callers treat it as "no match"
 * rather than crashing.
 */
export function originOf(url: string): string | null {
  try {
    return new URL(url).origin
  } catch {
    return null
  }
}

/**
 * Finds a saved CLI context whose URL points at the same origin (scheme,
 * host, and port) as `url`, if any. Used to decide whether a credential is
 * actually bound to a target -- see `ensureMcpAuth`'s doc comment for why
 * this matters.
 */
async function findContextByUrl(url: string): Promise<{ url: string; apiKey: string } | null> {
  const target = originOf(url)
  if (!target) return null
  const contexts = await listContexts()
  return contexts.find((c) => originOf(c.url) === target) ?? null
}

/**
 * Auth gate for the mcp command family. Unlike the plain `requireAuth()` used
 * by every other command (which just errors out with "run `temps login`"),
 * this offers to run the device-flow login right here -- the mcp wizard is
 * commonly someone's first CLI interaction on a machine, so making them stop,
 * remember the separate `login` command, and re-run `mcp add` is unnecessary
 * friction. Skipped entirely in `--yes` (non-interactive) mode, where the
 * caller must already have a valid context or pass `--api-key`.
 *
 * SECURITY: when `urlOverride` names a specific target, this must NEVER fall
 * back to whatever credential happens to be ambiently active for a
 * *different* server. `credentials.isAuthenticated()` / `requireAuth()` are
 * URL-agnostic -- they answer "is *some* session active", not "is a session
 * active *for this host*". Without this check, `mcp add --url <anything>`
 * (or the "Enter a different URL..." wizard prompt) would silently reuse the
 * active context's API key for whatever URL was given, which sends that key
 * as an Authorization header to it -- e.g. a mistyped or malicious URL would
 * receive a fully working credential for the real instance it belongs to.
 */
async function ensureMcpAuth(opts: { yes?: boolean; urlOverride?: string } = {}): Promise<string> {
  if (opts.urlOverride) {
    const matched = await findContextByUrl(opts.urlOverride)
    if (matched) {
      return matched.apiKey
    }

    if (opts.yes) {
      console.error(
        `Not authenticated to ${opts.urlOverride} and no --api-key provided. ` +
          'Refusing to reuse a credential from a different Temps instance.',
      )
      process.exit(1)
    }

    warning(`No saved login is bound to ${opts.urlOverride}.`)
    info("Your active session's key belongs to a different server and will not be reused for it.")
    const wantsLogin = await promptConfirm({
      message: `Log in to ${opts.urlOverride} now?`,
      default: true,
    })
    if (!wantsLogin) {
      console.error(`Not authenticated to ${opts.urlOverride}. Please run: temps login --url ${opts.urlOverride}`)
      process.exit(1)
    }

    await loginWithDevice({ url: opts.urlOverride })
    const ctx = await findContextByUrl(opts.urlOverride)
    if (ctx) return ctx.apiKey
    // loginWithDevice always establishes a context for the URL it was given;
    // this is an unreachable safety net, not an expected path.
    console.error(`Login to ${opts.urlOverride} did not produce a usable session.`)
    process.exit(1)
  }

  // No specific target named -- fall back to the ambient session, same as
  // every other CLI command.
  if (await credentials.isAuthenticated()) {
    return requireAuth()
  }
  if (opts.yes) {
    return requireAuth() // prints "Not authenticated..." and exits, same as before
  }

  warning('Not logged in to the Temps CLI yet.')
  const wantsLogin = await promptConfirm({
    message: 'Log in now?',
    default: true,
  })
  if (!wantsLogin) {
    return requireAuth() // reuses the standard "run `temps login`" exit
  }

  const url = await promptText({
    message: 'Temps server URL (leave blank to use the default)',
    default: getApiUrl(),
  })

  await loginWithDevice({ url: url || undefined })
  return requireAuth()
}

/**
 * Normalizes an explicit --url override to the same "web root, no /api
 * suffix" shape getWebUrl() returns, so callers can pass either form.
 * Undefined falls through to the existing context/env-resolved default.
 */
function resolveWebUrl(urlOverride: string | undefined): string {
  if (!urlOverride) return getWebUrl()
  return urlOverride.replace(/\/+$/, '').replace(/\/api$/, '')
}

const URL_OPTION_DESCRIPTION =
  'Target this Temps instance directly (e.g. copied from the Settings UI), without needing a saved CLI context or changing the active one. Defaults to the current context/login when omitted.'

/** Sentinel choice value for "type a URL instead of picking a saved context". */
const CUSTOM_TARGET = '__custom_url__'

interface McpTarget {
  url?: string
  apiKey?: string
}

/**
 * The wizard's first step: which Temps instance to configure. Defaults to
 * the active context (Enter reproduces today's behavior) so this never adds
 * friction for the common case, but makes the choice explicit instead of
 * silently trusting whatever happens to be active -- especially important
 * for `mcp add`, which can end up embedding the wrong server's API key into
 * an AI client's config if the wrong instance is picked (see addAction).
 *
 * Skipped (returns { url: options.url }) when --url was already passed
 * explicitly, in --yes mode (must stay non-interactive), or when there are
 * no saved contexts at all -- ensureMcpAuth's own "log in now?" prompt
 * covers that last case instead.
 */
async function selectMcpTarget(options: { url?: string; yes?: boolean }): Promise<McpTarget> {
  if (options.url || options.yes) return { url: options.url }

  const contexts = await listContexts()
  if (contexts.length === 0) return {}

  const active = await getActiveContext()
  // Active context first so it's always in view on the initial (unfiltered)
  // page, regardless of how many contexts are configured -- this is the
  // "just press Enter" fast path. Typing filters by name OR url (the search
  // term is matched against `description` too), which matters once there
  // are more contexts than fit on one page.
  const ordered = active ? [active, ...contexts.filter((c) => c.name !== active.name)] : contexts

  const selected = await promptSearch({
    message: 'Which Temps instance do you want to configure? (type to search)',
    choices: [
      ...ordered.map((c) => ({
        name: `${c.name}${active?.name === c.name ? '  [current]' : ''}`,
        value: c.name,
        description: c.url,
      })),
      { name: 'Enter a different URL...', value: CUSTOM_TARGET, alwaysShow: true },
    ],
  })

  if (selected === CUSTOM_TARGET) {
    const url = await promptText({ message: 'Temps server URL' })
    if (!url) return {}
    // If the typed URL happens to match a saved context (same host), bind
    // its known-good credential instead of leaving this unbound -- avoids
    // an unnecessary re-login and, more importantly, ensures the credential
    // ensureMcpAuth resolves later is actually known to belong to this URL
    // rather than falling through to an ambient key for a different server.
    const matched = await findContextByUrl(url)
    return matched ? { url, apiKey: matched.apiKey } : { url }
  }

  const ctx = await getContext(selected)
  return ctx ? { url: ctx.url, apiKey: ctx.apiKey } : {}
}

interface AddOptions {
  groups?: string
  write?: boolean
  yes?: boolean
  apiKey?: string
  url?: string
}

export function registerMcpCommands(program: Command): void {
  const mcp = program
    .command('mcp')
    .description('Configure this Temps instance as an MCP server for AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Windsurf, Zed)')

  mcp
    .command('enable')
    .description('Enable the Temps MCP server on this instance (admin, one-time per instance)')
    .option('-u, --url <url>', URL_OPTION_DESCRIPTION)
    .action(enableAction)

  mcp
    .command('disable')
    .description('Disable the Temps MCP server on this instance (admin)')
    .option('-u, --url <url>', URL_OPTION_DESCRIPTION)
    .action(disableAction)

  mcp
    .command('add [client]')
    .description(`Configure an AI client to connect to this Temps instance over MCP. Clients: ${listClientIds().join(', ')}`)
    .option('-g, --groups <groups>', 'Comma-separated tool groups to enable (default: all)')
    .option('-w, --write', 'Enable write tools (deploy, delete, restart, etc). Default: read-only')
    .option('-k, --api-key <key>', 'Use this API key instead of creating or prompting for one')
    .option('-u, --url <url>', URL_OPTION_DESCRIPTION)
    .option('-y, --yes', 'Skip prompts and confirmation (uses defaults; requires --api-key or an existing login)')
    .action(addAction)

  mcp
    .command('remove [client]')
    .description('Remove the Temps MCP server from an AI client')
    .action(removeAction)

  mcp
    .command('status')
    .description('Show whether this instance has MCP enabled and which AI clients are configured')
    .option('-u, --url <url>', URL_OPTION_DESCRIPTION)
    .action(statusAction)
}

async function setMcpServerEnabled(enabled: boolean, urlOverride?: string): Promise<void> {
  // The resolved key must actually be wired into setupClient -- passing it
  // no apiKeyOverride would fall through to the ambient credential
  // regardless of what ensureMcpAuth just verified for urlOverride.
  const apiKey = await ensureMcpAuth({ urlOverride })
  await setupClient(urlOverride, apiKey)

  await withSpinner(enabled ? 'Enabling the Temps MCP server...' : 'Disabling the Temps MCP server...', async () => {
    // PUT /settings replaces every field not present in the body with its
    // Rust default, so this must send the full current settings with only
    // mcp_server changed -- same rule as apps/temps-cli/src/commands/settings/index.ts.
    const { data: currentSettings, error: getError } = await getSettings({ client })
    if (getError) throw new Error(getErrorMessage(getError))

    const { error } = await updateSettings({
      client,
      body: { ...currentSettings, mcp_server: { enabled } } as AppSettings,
    })
    if (error) throw new Error(getErrorMessage(error))
  })
}

async function enableAction(options: { url?: string }): Promise<void> {
  await setMcpServerEnabled(true, options.url)
  success('MCP server enabled.')
  info('Run `bunx @temps-sdk/cli mcp add <client>` to connect an AI client (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Windsurf, Zed).')
}

async function disableAction(options: { url?: string }): Promise<void> {
  await setMcpServerEnabled(false, options.url)
  success('MCP server disabled.')
  info('AI clients configured with `mcp add` will stop working until this is enabled again.')
}

async function addAction(clientArg: string | undefined, options: AddOptions): Promise<void> {
  const target = await selectMcpTarget({ url: options.url, yes: options.yes })
  // The credential ensureMcpAuth resolves here is guaranteed bound to
  // target.url (or the process exits) -- never an ambient key for a
  // different server. Reused below instead of re-deriving via requireAuth().
  const authenticatedApiKey = await ensureMcpAuth({ yes: options.yes, urlOverride: target.url })
  await setupClient(target.url, target.apiKey ?? authenticatedApiKey)

  const clientId =
    clientArg ||
    (await promptSelect({
      message: 'Which AI client do you want to configure?',
      choices: CLIENT_ADAPTERS.map((c) => ({ name: c.label, value: c.id })),
    }))

  const adapter = getClientAdapter(clientId)
  if (!adapter) {
    warning(`Unknown client "${clientId}". Available: ${listClientIds().join(', ')}`)
    return
  }

  // MCP endpoints (/mcp, /mcp/tools) are mounted at the server root, not under
  // /api like the REST API -- getApiUrl() (used by the generated client) would
  // build a URL that always 404s here. resolveWebUrl() strips the /api suffix
  // (and honors the instance chosen above, whether via --url or the picker).
  const mcpBaseUrl = resolveWebUrl(target.url)

  const probe = await withSpinner('Checking for Temps MCP support...', () => probeMcpEndpoint(mcpBaseUrl))
  if (!probe.supported) {
    warning(`This Temps instance does not expose an MCP endpoint (${probe.reason}).`)
    info('Run `bunx @temps-sdk/cli mcp enable` as an admin to turn it on (or this instance may predate MCP support).')
    return
  }

  const clientSupported = await adapter.isClientSupported()
  if (!clientSupported) {
    warning(`${adapter.label} was not detected on this machine -- continuing anyway.`)
  }

  let groups: string[]
  if (options.groups) {
    groups = options.groups
      .split(',')
      .map((g) => g.trim())
      .filter(Boolean)
    const invalid = groups.filter((g) => !isValidGroupKey(g))
    if (invalid.length > 0) {
      warning(`Unknown tool group(s): ${invalid.join(', ')}. Available: ${TOOL_GROUPS.map((g) => g.key).join(', ')}`)
      return
    }
  } else if (options.yes) {
    groups = TOOL_GROUPS.map((g) => g.key)
  } else {
    groups = await promptCheckbox({
      message: 'Which tool groups should be available?',
      choices: TOOL_GROUPS.map((g) => ({
        name: `${g.label} (${g.categories.length} categories)`,
        value: g.key,
      })),
    })
    if (groups.length === 0) groups = TOOL_GROUPS.map((g) => g.key)
  }

  const write =
    options.write ??
    (options.yes
      ? false
      : await promptConfirm({
          message:
            'Allow write tools (deploy, delete, restart, etc)? A Temps instance controls real infrastructure -- ' +
            'even with this on, every write requires an explicit confirmation in the AI client before it executes.',
          default: false,
        }))

  let apiKey = options.apiKey
  if (!apiKey) {
    const wantsNewKey = options.yes
      ? false
      : await promptConfirm({
          message: 'Create a new dedicated API key scoped for MCP access? (recommended over reusing an existing key)',
          default: true,
        })

    if (wantsNewKey) {
      const created = await withSpinner('Creating a scoped API key...', async () => {
        const { data, error } = await createApiKey({
          client,
          body: {
            name: `mcp-${adapter.id}-${new Date().toISOString().slice(0, 10)}`,
            role_type: write ? 'user' : 'reader',
            expires_at: null,
            permissions: null,
          },
        })
        if (error) throw new Error(getErrorMessage(error))
        if (!data) throw new Error('No response data from create API key')
        return data
      })
      apiKey = created.api_key
      newline()
      success(`Created API key "${created.name}" (${created.key_prefix}...)`)
    } else {
      // authenticatedApiKey (from ensureMcpAuth, above) is already bound to
      // target.url -- never requireAuth()'s ambient resolution, which
      // reflects whatever context is globally active and could be a
      // *different* server than the one just chosen, which would otherwise
      // embed the wrong credentials into this client's config.
      apiKey = target.apiKey ?? authenticatedApiKey
    }
  }

  const url = buildMcpUrl(mcpBaseUrl, groups, write)

  newline()
  header(`Configure ${adapter.label}`)
  keyValue('Target', await adapter.describeTarget())
  keyValue('URL', url)
  keyValue('Groups', groups.length === TOOL_GROUPS.length ? 'all' : groups.join(', '))
  keyValue('Write tools', write ? 'enabled' : 'disabled (read-only)')
  newline()

  if (!options.yes) {
    const proceed = await promptConfirm({ message: 'Apply this configuration?', default: true })
    if (!proceed) {
      info('Cancelled -- no changes made.')
      return
    }
  }

  const result = await adapter.addServer({ url, apiKey })
  if (!result.success) {
    warning(`Failed to configure ${adapter.label}: ${result.reason}`)
    return
  }

  if (result.alreadyInstalled) {
    info(`${adapter.label} is already configured with this exact setup.`)
  } else {
    success(`${adapter.label} configured.`)
  }
  info(`Restart ${adapter.label} to pick up the change.`)

  // Codex resolves its bearer token from its OWN process environment at
  // connect time -- config.toml only stores the env var's name, never the
  // value (see codex.ts's TOKEN_ENV_VAR doc comment). Without this export,
  // `codex mcp add` reports success but real codex sessions will silently
  // fail to authenticate.
  if (adapter.id === 'codex') {
    newline()
    warning(`Codex reads its bearer token from the ${CODEX_TOKEN_ENV_VAR} environment variable at connect time -- export it in your shell profile, or every codex session will fail to authenticate to Temps:`)
    info(`  export ${CODEX_TOKEN_ENV_VAR}='Bearer ${apiKey}'`)
  }
}

async function removeAction(clientArg: string | undefined): Promise<void> {
  const clientId =
    clientArg ||
    (await promptSelect({
      message: 'Which AI client do you want to remove the Temps MCP server from?',
      choices: CLIENT_ADAPTERS.map((c) => ({ name: c.label, value: c.id })),
    }))

  const adapter = getClientAdapter(clientId)
  if (!adapter) {
    warning(`Unknown client "${clientId}". Available: ${listClientIds().join(', ')}`)
    return
  }

  const result = await adapter.removeServer()
  if (!result.success) {
    warning(`Failed to remove from ${adapter.label}: ${result.reason}`)
    return
  }

  if (result.alreadyInstalled) {
    info(`${adapter.label} had no Temps MCP server configured.`)
  } else {
    success(`Removed the Temps MCP server from ${adapter.label}.`)
  }
}

interface StatusRow {
  label: string
  detected: string
  installed: string
  groups: string
}

/**
 * Summarizes which tool groups + write mode a client's configured URL grants,
 * for the "Tool groups" status column. Returns "-" when the client isn't
 * configured, or when its URL wasn't built by this CLI (an entry hand-edited
 * or written by another tool -- e.g. the legacy @temps-sdk/mcp package --
 * doesn't carry the groups/write query params this parses).
 */
function formatGroups(url: string | null): string {
  if (!url) return '-'
  const parsed = parseMcpUrl(url)
  if (!parsed) return '-'
  const label = parsed.groups.length === TOOL_GROUPS.length ? 'All' : `${parsed.groups.length}/${TOOL_GROUPS.length}`
  return parsed.write ? `${label} (write)` : label
}

async function statusAction(options: { url?: string }): Promise<void> {
  const mcpBaseUrl = resolveWebUrl(options.url)
  const probe = await withSpinner('Checking this instance...', () => probeMcpEndpoint(mcpBaseUrl))

  header('Temps MCP status')
  newline()
  keyValue('This instance', probe.supported ? `${icons.success} enabled (${mcpBaseUrl})` : `${icons.bullet} disabled (${mcpBaseUrl})`)
  if (!probe.supported) {
    info('Run `bunx @temps-sdk/cli mcp enable` as an admin to turn it on.')
  }
  newline()

  const rows: StatusRow[] = await Promise.all(
    CLIENT_ADAPTERS.map(async (adapter) => {
      const [detected, installed] = await Promise.all([adapter.isClientSupported(), adapter.isServerInstalled()])
      const url = installed ? await adapter.getServerUrl() : null
      return {
        label: adapter.label,
        detected: detected ? `${icons.success} detected` : `${icons.bullet} not detected`,
        installed: installed ? `${icons.success} configured` : `${icons.bullet} not configured`,
        groups: formatGroups(url),
      }
    }),
  )

  const columns: TableColumn<StatusRow>[] = [
    { header: 'Client', key: 'label' },
    { header: 'On this machine', key: 'detected' },
    { header: 'Temps MCP server', key: 'installed' },
    { header: 'Tool groups', key: 'groups' },
  ]

  printTable(rows, columns)
}
