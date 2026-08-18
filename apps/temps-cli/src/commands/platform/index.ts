import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getPlatformInfo,
  getAccessInfo,
  getPrivateIp,
  getPublicIp,
  getUpdateCapability,
  getUpdateStatus,
  startUpdate,
  checkForUpdate,
  getSettings,
  updateSettings,
} from '../../api/sdk.gen.js'
import { platformAlertRulesList, platformAlertRulesSet } from './alert-rules.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, info, keyValue, success, warning, error, formatDate } from '../../ui/output.js'

export function registerPlatformCommands(program: Command): void {
  const platform = program
    .command('platform')
    .alias('plat')
    .description('View platform and server information')

  platform
    .command('info')
    .description('Get platform information')
    .option('--json', 'Output in JSON format')
    .action(platformInfoAction)

  platform
    .command('access')
    .description('Get access and networking information')
    .option('--json', 'Output in JSON format')
    .action(accessInfoAction)

  platform
    .command('private-ip')
    .description('Get the server private IP address')
    .action(privateIpAction)

  platform
    .command('public-ip')
    .description('Get the server public IP address')
    .action(publicIpAction)

  const update = platform
    .command('update')
    .description('Check for and apply temps releases on the server')

  update
    .command('status')
    .description('Show the available release and whether it can be applied from here')
    .option('--json', 'Output in JSON format')
    .action(updateStatusAction)

  update
    .command('check')
    .description('Ask the release API for the newest version on this channel now')
    .option('--json', 'Output in JSON format')
    .action(updateCheckAction)

  update
    .command('channel [channel]')
    .description(
      'Show or set the release channel: stable, beta, nightly, or "auto" to follow the installed version'
    )
    .option('--json', 'Output in JSON format')
    .action(updateChannelAction)

  update
    .command('apply')
    .description('Install a release on the server and restart it')
    .option('--version <version>', 'Release tag to install (default: newest on this channel)')
    .option('-y, --yes', 'Skip the confirmation prompt')
    .option('--json', 'Output in JSON format')
    .action(updateApplyAction)

  // Node-scoped monitoring alert rules — the control-plane's own proxy and
  // file-descriptor/socket-exhaustion alerts, which belong to no project.
  const alertRules = platform
    .command('alert-rules')
    .description("Inspect and retune the control-plane's own monitoring alert rules")

  alertRules
    .command('list')
    .description('List the alert rules watching this node (proxy health, socket exhaustion)')
    .option('--node <id>', 'Node ID (default: 0, the control plane)')
    .option('--json', 'Output in JSON format')
    .action(platformAlertRulesList)

  alertRules
    .command('set <rule-id>')
    .description('Retune, enable, or disable an alert rule on this node')
    .option('--node <id>', 'Node ID (default: 0, the control plane)')
    .option('--threshold <n>', 'Value the metric must cross to fire')
    .option('--comparator <op>', 'Comparison operator: >, >=, <, <=')
    .option('--severity <level>', 'Alert severity: warning or critical')
    .option('--for-duration <secs>', 'Seconds the condition must hold before firing')
    .option('--enable', 'Enable the rule')
    .option('--disable', 'Disable the rule (survives the startup re-seed; deleting does not)')
    .option('--json', 'Output in JSON format')
    .action(platformAlertRulesSet)

  alertRules.addHelpText(
    'after',
    `
These rules are seeded automatically on server startup and are update-only:
a deleted rule reappears on the next restart, so use --disable to stop one.

Examples:
  $ temps platform alert-rules list
  $ temps platform alert-rules set 39 --threshold 80
  $ temps platform alert-rules set 39 --disable`
  )
}

async function platformInfoAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const platformInfo = await withSpinner('Fetching platform info...', async () => {
    const { data, error } = await getPlatformInfo({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!platformInfo) {
    info('Platform information not available')
    return
  }

  if (options.json) {
    json(platformInfo)
    return
  }

  newline()
  header(`${icons.globe} Platform Information`)
  keyValue('OS Type', platformInfo.os_type)
  keyValue('Architecture', platformInfo.architecture)
  keyValue('Platforms', platformInfo.platforms.join(', ') || colors.muted('none'))
  newline()
}

async function accessInfoAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const accessData = await withSpinner('Fetching access info...', async () => {
    const { data, error } = await getAccessInfo({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!accessData) {
    info('Access information not available')
    return
  }

  if (options.json) {
    json(accessData)
    return
  }

  newline()
  header(`${icons.globe} Access Information`)
  keyValue('Access Mode', accessData.access_mode)
  keyValue('Can Create Domains', accessData.can_create_domains ? colors.success('Yes') : colors.muted('No'))
  if (accessData.domain_creation_error) {
    keyValue('Domain Error', colors.warning(accessData.domain_creation_error))
  }
  keyValue('Public IP', accessData.public_ip || colors.muted('not available'))
  keyValue('Private IP', accessData.private_ip || colors.muted('not available'))
  newline()
}

async function privateIpAction(): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching private IP...', async () => {
    const { data, error } = await getPrivateIp({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const ip = extractIpValue(result)
  if (ip !== null) {
    success(ip)
  } else {
    json(result)
  }
}

async function publicIpAction(): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching public IP...', async () => {
    const { data, error } = await getPublicIp({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  const ip = extractIpValue(result)
  if (ip !== null) {
    success(ip)
  } else {
    json(result)
  }
}

/**
 * The IP endpoints return either `{ ip: "..." }`, a bare string, or (rarely)
 * something else entirely. Returning null tells the caller to fall back to
 * printing raw JSON instead of guessing at a shape that isn't there.
 */
export function extractIpValue(result: unknown): string | null {
  if (result && typeof result === 'object' && 'ip' in result) {
    return String((result as { ip: string }).ip)
  }
  if (typeof result === 'string') {
    return result
  }
  return null
}

/**
 * Report both halves of the picture: whether a newer release exists, and
 * whether this install can apply it without someone SSH-ing to the host.
 */
async function updateStatusAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const { status, capability } = await withSpinner('Checking for updates...', async () => {
    const [statusRes, capabilityRes] = await Promise.all([
      getUpdateStatus({ client }),
      getUpdateCapability({ client }),
    ])
    if (statusRes.error) {
      throw new Error(getErrorMessage(statusRes.error))
    }
    if (capabilityRes.error) {
      throw new Error(getErrorMessage(capabilityRes.error))
    }
    return { status: statusRes.data, capability: capabilityRes.data }
  })

  if (options.json) {
    json({ status, capability })
    return
  }

  newline()
  header(`${icons.globe} Platform Updates`)

  if (status?.update_available) {
    keyValue('Current', status.current_version ?? colors.muted('unknown'))
    keyValue('Available', colors.success(status.latest_version ?? 'unknown'))
    keyValue('Channel', status.channel ?? colors.muted('unknown'))
    if (status.release_url) {
      keyValue('Release notes', status.release_url)
    }
  } else {
    keyValue('Status', 'Up to date (or no check has completed yet)')
    keyValue('Current', status?.current_version ?? colors.muted('unknown'))
  }

  newline()
  keyValue('Supervisor', capability?.supervisor ?? colors.muted('unknown'))
  keyValue('Binary', capability?.binary_path || colors.muted('unknown'))

  if (capability?.can_apply && capability.allowed) {
    keyValue('Apply from here', colors.success('Yes'))
    info(`Run ${colors.primary('temps platform update apply')} to install and restart.`)
  } else {
    keyValue('Apply from here', colors.warning('No'))
    if (!capability?.allowed) {
      warning('Your credentials lack the platform:update permission.')
    }
    if (capability?.reason) {
      warning(capability.reason)
    }
    // Never leave the operator without a way forward.
    info(`Upgrade manually on the host: ${colors.primary(capability?.manual_command ?? 'temps upgrade')}`)
  }

  if (capability?.caveat) {
    warning(capability.caveat)
  }

  // Surface how the previous attempt ended — especially a failure, which the
  // person running this may never have seen if it happened in the console.
  const attempt = capability?.last_attempt
  if (attempt) {
    newline()
    header(`${icons.bullet} Last update attempt`)
    keyValue('Result', attempt.status === 'succeeded'
      ? colors.success('succeeded')
      : attempt.status === 'failed'
        ? colors.error('failed')
        : 'in progress')
    keyValue('From', attempt.from_version)
    keyValue('To', attempt.to_version ?? colors.muted('not resolved'))
    keyValue('Started', formatDate(attempt.started_at))
    if (attempt.error) {
      warning(attempt.error)
    }
    if (attempt.previous_binary_path) {
      keyValue('Previous binary', attempt.previous_binary_path)
    }
  }
  newline()
}

/**
 * Apply a release. The server restarts itself, so this deliberately does NOT
 * wait for a response body beyond the acceptance — it polls afterwards and
 * reports the recorded outcome once the server answers again.
 */
async function updateApplyAction(options: {
  version?: string
  yes?: boolean
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  const capability = await withSpinner('Checking update capability...', async () => {
    const { data, error: err } = await getUpdateCapability({ client })
    if (err) {
      throw new Error(getErrorMessage(err))
    }
    return data
  })

  // Fail before prompting when the server has already told us it cannot.
  if (!capability?.can_apply || !capability.allowed) {
    error('This server cannot apply updates right now.')
    if (!capability?.allowed) {
      warning('Your credentials lack the platform:update permission.')
    }
    if (capability?.reason) {
      warning(capability.reason)
    }
    info(`Upgrade manually on the host: ${colors.primary(capability?.manual_command ?? 'temps upgrade')}`)
    process.exitCode = 1
    return
  }

  if (capability.caveat) {
    warning(capability.caveat)
  }

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message: `Install ${options.version ?? 'the latest release'} and restart the server? It will be briefly unavailable.`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled.')
      return
    }
  }

  const started = await withSpinner('Starting update...', async () => {
    const { data, error: err } = await startUpdate({
      client,
      body: options.version ? { version: options.version } : {},
    })
    if (err) {
      throw new Error(getErrorMessage(err))
    }
    return data
  })

  if (options.json) {
    json(started)
    return
  }

  success(`Update started from ${started?.current_version ?? 'the running version'}.`)
  info('The server is downloading, verifying and installing, then restarting.')

  const outcome = await withSpinner('Waiting for the server to come back...', async () =>
    waitForUpdateOutcome(
      capability.last_attempt?.started_at ?? null,
      (started?.estimated_restart_secs ?? 45) + 60
    )
  )

  newline()
  if (!outcome) {
    warning('The server did not report an outcome in time.')
    info('Check the service on the host (systemctl status temps / journalctl -u temps).')
    process.exitCode = 1
    return
  }
  if (outcome.status === 'succeeded') {
    success(`temps is now running ${outcome.to_version} (was ${outcome.from_version}).`)
    return
  }
  error(outcome.error ?? 'The update did not complete.')
  info(`The server is still running ${outcome.from_version}.`)
  if (outcome.previous_binary_path) {
    info(`Previous binary kept at ${outcome.previous_binary_path}`)
  }
  process.exitCode = 1
}

/**
 * Poll until the attempt we just started resolves.
 *
 * Connection errors are expected and ignored — the server is restarting, which
 * is the whole point. `baselineStartedAt` distinguishes our attempt from a
 * result left over from a previous one.
 */
async function waitForUpdateOutcome(
  baselineStartedAt: string | null,
  timeoutSecs: number
): Promise<
  | {
      status: string
      from_version: string
      to_version?: string | null
      error?: string | null
      previous_binary_path?: string | null
    }
  | null
> {
  const deadline = Date.now() + timeoutSecs * 1000
  while (Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 2000))
    try {
      const { data } = await getUpdateCapability({ client })
      const attempt = data?.last_attempt
      if (isTerminalUpdateAttempt(attempt, baselineStartedAt)) {
        return attempt as NonNullable<typeof attempt>
      }
    } catch {
      // Server is down mid-restart; keep waiting.
    }
  }
  return null
}

/**
 * True when `attempt` is both terminal (not "pending") and belongs to the
 * attempt we just kicked off, not a stale one left over from before this
 * poll started. Guards against reporting a leftover failed/succeeded attempt
 * as if it were the outcome of the update we're currently waiting on.
 */
export function isTerminalUpdateAttempt(
  attempt: { status: string; started_at: string } | null | undefined,
  baselineStartedAt: string | null
): boolean {
  return Boolean(
    attempt && attempt.status !== 'pending' && attempt.started_at !== baselineStartedAt
  )
}

/** Valid channels, plus the sentinel that clears an explicit pin. */
const UPDATE_CHANNELS = ['stable', 'beta', 'nightly'] as const
const CHANNEL_AUTO = 'auto'

/**
 * Normalizes and validates a requested channel. Returns `{ error }` instead
 * of throwing so the caller can print the exact same message via the CLI's
 * error() helper and set the conventional exit code, unchanged from before
 * this was extracted.
 */
export function parseUpdateChannel(
  channel: string
): { requested: string; isAuto: boolean } | { error: string } {
  const requested = channel.trim().toLowerCase()
  const isAuto = requested === CHANNEL_AUTO
  if (!isAuto && !UPDATE_CHANNELS.includes(requested as (typeof UPDATE_CHANNELS)[number])) {
    return {
      error: `Unknown channel '${channel}'. Use one of: ${UPDATE_CHANNELS.join(', ')}, or ${CHANNEL_AUTO}.`,
    }
  }
  return { requested, isAuto }
}

/**
 * Settings PUT replaces the whole document. Defaulting `enabled` to `true`
 * when it's missing means switching channel never silently disables
 * self-update as a side effect.
 */
export function buildSelfUpdateSettings(
  current: { self_update?: { enabled?: boolean | null } | null },
  requested: string,
  isAuto: boolean
): { enabled: boolean; channel: string | null } {
  return {
    enabled: current.self_update?.enabled ?? true,
    channel: isAuto ? null : requested,
  }
}

/** Force a release check instead of waiting for the server's periodic one. */
async function updateCheckAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Checking the release API...', async () => {
    const { data, error: err } = await checkForUpdate({ client })
    if (err) {
      throw new Error(getErrorMessage(err))
    }
    return data
  })

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.globe} Release Check`)
  keyValue('Channel', result?.channel ?? colors.muted('unknown'))
  keyValue('Current', result?.current_version ?? colors.muted('unknown'))
  keyValue('Latest', result?.latest_version ?? colors.muted('none published'))
  if (result?.update_available) {
    success('An update is available. Run `temps platform update apply` to install it.')
    if (result.release_url) {
      keyValue('Release notes', result.release_url)
    }
  } else {
    // Not necessarily "nothing newer exists" — on a more stable channel the
    // newest release is often older than a nightly build, which is expected.
    info('Nothing newer on this channel.')
  }
  newline()
}

/** Show or pin the release channel this server tracks. */
async function updateChannelAction(
  channel: string | undefined,
  options: { json?: boolean }
): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!channel) {
    const capability = await withSpinner('Fetching channel...', async () => {
      const { data, error: err } = await getUpdateCapability({ client })
      if (err) {
        throw new Error(getErrorMessage(err))
      }
      return data
    })
    if (options.json) {
      json({
        channel: capability?.channel,
        pinned: capability?.channel_is_pinned,
      })
      return
    }
    newline()
    keyValue('Channel', capability?.channel ?? colors.muted('unknown'))
    keyValue(
      'Source',
      capability?.channel_is_pinned
        ? 'pinned in settings'
        : 'inferred from the installed version'
    )
    newline()
    return
  }

  const parsedChannel = parseUpdateChannel(channel)
  if ('error' in parsedChannel) {
    error(parsedChannel.error)
    process.exitCode = 1
    return
  }
  const { requested, isAuto } = parsedChannel

  await withSpinner('Updating channel...', async () => {
    // The settings PUT replaces the whole document, so read-modify-write is
    // required — sending only `self_update` would reset every other field.
    const { data: current, error: readErr } = await getSettings({ client })
    if (readErr || !current) {
      throw new Error(readErr ? getErrorMessage(readErr) : 'Could not read settings')
    }
    const { error: writeErr } = await updateSettings({
      client,
      body: {
        ...current,
        self_update: buildSelfUpdateSettings(current, requested, isAuto),
      } as never,
    })
    if (writeErr) {
      throw new Error(getErrorMessage(writeErr))
    }
  })

  success(
    isAuto
      ? 'Channel now follows the installed version.'
      : `Now tracking the ${requested} channel.`
  )
  info('Run `temps platform update check` to look for releases on it.')
}
