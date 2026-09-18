// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getGeoDatabaseStatus,
  getSettings,
  updateSettings,
} from '../../api/sdk.gen.js'
import type {
  AppSettings,
  GeoDatabaseStatusResponse,
  GeoSettingsMasked,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptText, promptConfirm, promptSelect, promptNumber, promptPassword } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface UpdateOptions {
  setting?: string
  value?: string
  externalUrl?: string
  previewDomain?: string
  letsencryptEmail?: string
  letsencryptMode?: string
  rateLimitingEnabled?: string
  rateLimitingRpm?: string
  screenshotsEnabled?: string
  maxRequestTimeout?: string
  defaultHttpTimeout?: string
  defaultSseIdleTimeout?: string
  defaultWebsocketIdleTimeout?: string
  maxMemoryLimitMb?: string
  maxConcurrentConnectionsCeiling?: string
  allowUnlimitedTimeouts?: string
  consoleForceHttps?: string
  yes?: boolean
}

interface SetExternalUrlOptions {
  url: string
}

interface SetPreviewDomainOptions {
  domain: string
}

/**
 * Slice of settings this function actually reads back, kept structural
 * rather than `AppSettings`/`AppSettingsResponse` so it accepts either (the
 * response type masks some unrelated fields, e.g. dns_provider).
 */
export interface CurrentSettingsSnapshot {
  letsencrypt?: { email?: string | null; environment?: string } | null
  rate_limiting?: { max_requests_per_minute?: number } | null
  request_timeouts?: {
    max_request_timeout_seconds?: number
    default_http_timeout_seconds?: number
    default_sse_idle_timeout_seconds?: number
    default_websocket_idle_timeout_seconds?: number
  } | null
  tenant_resource_ceilings?: {
    max_memory_limit_mb?: number
    max_concurrent_connections?: number
    allow_unlimited_request_timeouts?: boolean
  } | null
  geo?: GeoSettingsMasked | null
}

/**
 * Builds the settings patch for non-interactive (`-y`) updates. Falls back to
 * the currently-configured value for fields that share a nested object (e.g.
 * letsencrypt, rate_limiting) so a partial flag like --letsencrypt-mode alone
 * doesn't blow away an existing email. Returns an error instead of throwing
 * so the caller can warn and abort without making an API call.
 */
export function buildAutomationSettingsUpdate(
  options: UpdateOptions,
  currentSettings: CurrentSettingsSnapshot | undefined,
): { updates: Partial<AppSettings> } | { error: string } {
  const updates: Partial<AppSettings> = {}

  if (options.externalUrl) {
    updates.external_url = options.externalUrl
  }
  if (options.previewDomain) {
    updates.preview_domain = options.previewDomain
  }
  if (options.letsencryptEmail || options.letsencryptMode) {
    updates.letsencrypt = {
      email: options.letsencryptEmail || currentSettings?.letsencrypt?.email || '',
      environment: options.letsencryptMode || currentSettings?.letsencrypt?.environment || 'staging',
    }
  }
  if (options.rateLimitingEnabled !== undefined) {
    const enabled = options.rateLimitingEnabled === 'true'
    updates.rate_limiting = {
      enabled,
      max_requests_per_minute: options.rateLimitingRpm ? parseInt(options.rateLimitingRpm, 10) : (currentSettings?.rate_limiting?.max_requests_per_minute || 60),
    }
  }
  if (options.consoleForceHttps !== undefined) {
    // Tri-state, matching an environment's force_https: "auto" clears the
    // override so the console inherits the per-host certificate heuristic.
    switch (options.consoleForceHttps) {
      case 'auto':
        updates.console_force_https = null
        break
      case 'always':
        updates.console_force_https = true
        break
      case 'never':
        updates.console_force_https = false
        break
      default:
        return {
          error: `--console-force-https must be auto, always or never, got "${options.consoleForceHttps}"`,
        }
    }
  }
  if (options.screenshotsEnabled !== undefined) {
    const enabled = options.screenshotsEnabled === 'true'
    updates.screenshots = {
      enabled,
    }
  }
  if (
    options.maxRequestTimeout ||
    options.defaultHttpTimeout ||
    options.defaultSseIdleTimeout ||
    options.defaultWebsocketIdleTimeout
  ) {
    const current = currentSettings?.request_timeouts
    const fields: Array<[string, string | undefined, number]> = [
      ['--max-request-timeout', options.maxRequestTimeout, current?.max_request_timeout_seconds ?? 600],
      ['--default-http-timeout', options.defaultHttpTimeout, current?.default_http_timeout_seconds ?? 0],
      ['--default-sse-idle-timeout', options.defaultSseIdleTimeout, current?.default_sse_idle_timeout_seconds ?? 0],
      ['--default-websocket-idle-timeout', options.defaultWebsocketIdleTimeout, current?.default_websocket_idle_timeout_seconds ?? 0],
    ]
    const parsed: number[] = []
    for (const [flag, raw, fallback] of fields) {
      if (raw === undefined) {
        parsed.push(fallback)
        continue
      }
      const value = parseInt(raw, 10)
      if (Number.isNaN(value)) {
        return { error: `${flag} must be a number, got "${raw}"` }
      }
      parsed.push(value)
    }
    updates.request_timeouts = {
      max_request_timeout_seconds: parsed[0],
      default_http_timeout_seconds: parsed[1],
      default_sse_idle_timeout_seconds: parsed[2],
      default_websocket_idle_timeout_seconds: parsed[3],
    }
  }

  // The ceilings share one nested object, so a single flag must carry the
  // other two forward — otherwise setting a memory ceiling would silently
  // drop an existing connection ceiling back to "no ceiling".
  if (
    options.maxMemoryLimitMb !== undefined ||
    options.maxConcurrentConnectionsCeiling !== undefined ||
    options.allowUnlimitedTimeouts !== undefined
  ) {
    const current = currentSettings?.tenant_resource_ceilings
    const numeric: Array<[string, string | undefined, number]> = [
      ['--max-memory-limit-mb', options.maxMemoryLimitMb, current?.max_memory_limit_mb ?? 0],
      ['--max-concurrent-connections-ceiling', options.maxConcurrentConnectionsCeiling, current?.max_concurrent_connections ?? 0],
    ]
    const parsed: number[] = []
    for (const [flag, raw, fallback] of numeric) {
      if (raw === undefined) {
        parsed.push(fallback)
        continue
      }
      const value = parseInt(raw, 10)
      if (Number.isNaN(value) || value < 0) {
        return { error: `${flag} must be a non-negative number (0 = no ceiling), got "${raw}"` }
      }
      parsed.push(value)
    }
    let allowUnlimited = current?.allow_unlimited_request_timeouts ?? true
    if (options.allowUnlimitedTimeouts !== undefined) {
      if (options.allowUnlimitedTimeouts !== 'true' && options.allowUnlimitedTimeouts !== 'false') {
        return { error: `--allow-unlimited-timeouts must be true or false, got "${options.allowUnlimitedTimeouts}"` }
      }
      allowUnlimited = options.allowUnlimitedTimeouts === 'true'
    }
    updates.tenant_resource_ceilings = {
      max_memory_limit_mb: parsed[0],
      max_concurrent_connections: parsed[1],
      allow_unlimited_request_timeouts: allowUnlimited,
    }
  }

  if (options.setting && options.value) {
    switch (options.setting) {
      case 'external_url':
        updates.external_url = options.value
        break
      case 'preview_domain':
        updates.preview_domain = options.value
        break
      default:
        return { error: `Unknown setting: ${options.setting}` }
    }
  }

  if (Object.keys(updates).length === 0) {
    return { error: 'No settings to update' }
  }

  return { updates }
}

/**
 * Slice of `GET /geo/status` this command actually reads, kept structural so
 * the formatter can be unit-tested without constructing the whole response.
 */
export type GeoStatusSnapshot = Pick<
  GeoDatabaseStatusResponse,
  'license_key_configured' | 'refresh_interval_hours' | 'is_stale'
> &
  Partial<
    Pick<
      GeoDatabaseStatusResponse,
      'last_check_status' | 'last_check_at' | 'last_error' | 'age_days' | 'source'
    >
  >

/**
 * One-line answer to "is my geolocation data current, and if not, why?".
 *
 * Every branch names the next action, because the three reasons a database
 * goes stale are indistinguishable from the raw fields: a failing download, a
 * refresh that is not licensed to run at all, and an instance that has simply
 * never checked. `level` maps to how the caller renders it, so an unlicensed
 * instance is a warning with a fix rather than a silent blank.
 */
export function describeGeoRefreshState(status: GeoStatusSnapshot): {
  level: 'ok' | 'warn'
  headline: string
  detail: string
} {
  const cadence = `every ${status.refresh_interval_hours} hours`

  if (status.last_check_status === 'skipped_no_license_key') {
    return {
      level: 'warn',
      headline: 'Automatic refreshes are not running',
      detail: `No MaxMind license key is configured, so the scheduled check (${cadence}) downloads nothing. Lookups keep using the database already on disk. Set a key with: temps settings update --setting geo`,
    }
  }
  if (status.last_check_status === 'error') {
    return {
      level: 'warn',
      headline: 'Last refresh check failed',
      detail: `${status.last_error ?? 'No reason was recorded'}. Retried ${cadence}; the previously loaded database stays in use until a check succeeds.`,
    }
  }
  if (!status.last_check_at) {
    return {
      level: status.license_key_configured ? 'ok' : 'warn',
      headline: 'No refresh has run yet on this instance',
      detail: status.license_key_configured
        ? `The scheduled job checks ${cadence}. Until then, lookups use whatever database is on disk.`
        : `No MaxMind license key is configured, so the scheduled job will not download anything. Set one with: temps settings update --setting geo`,
    }
  }
  if (status.is_stale) {
    return {
      level: 'warn',
      headline: 'Geolocation data is stale',
      detail: `The loaded database is ${status.age_days ?? 'an unknown number of'} days old. Checks run ${cadence}; a license key makes them fetch MaxMind's latest build.`,
    }
  }

  return {
    level: 'ok',
    headline: 'Geolocation data is current',
    detail: `${status.age_days ?? 0} days old, checked ${cadence} from ${status.source ?? 'an unknown source'}.`,
  }
}

export function registerSettingsCommands(program: Command): void {
  const settings = program
    .command('settings')
    .description('Manage platform settings')

  settings
    .command('show')
    .alias('get')
    .description('Show current platform settings')
    .option('--json', 'Output in JSON format')
    .action(showSettings)

  settings
    .command('update')
    .alias('set')
    .description('Update platform settings')
    .option('-s, --setting <setting>', 'Setting to update (external_url, preview_domain, letsencrypt, rate_limiting, security_headers, screenshots)')
    .option('-v, --value <value>', 'Value for the setting')
    .option('--external-url <url>', 'External URL for the platform')
    .option('--preview-domain <domain>', 'Preview domain pattern')
    .option('--letsencrypt-email <email>', 'Let\'s Encrypt email')
    .option('--letsencrypt-mode <mode>', 'Let\'s Encrypt mode (staging, production)')
    .option('--rate-limiting-enabled <enabled>', 'Enable rate limiting (true/false)')
    .option('--rate-limiting-rpm <rpm>', 'Requests per minute')
    .option('--screenshots-enabled <enabled>', 'Enable screenshots (true/false)')
    .option('--max-request-timeout <seconds>', 'Hard ceiling for all upstream request/idle timeouts, in seconds')
    .option('--default-http-timeout <seconds>', 'Default timeout for regular HTTP requests, in seconds')
    .option('--default-sse-idle-timeout <seconds>', 'Default idle timeout for SSE streams, in seconds')
    .option('--default-websocket-idle-timeout <seconds>', 'Default idle timeout for WebSocket connections, in seconds')
    .option('--max-memory-limit-mb <mb>', 'Ceiling on a project/environment memory limit override, in MB (0 = no ceiling)')
    .option('--max-concurrent-connections-ceiling <count>', 'Ceiling on a project/environment concurrent-connection override (0 = no ceiling)')
    .option('--allow-unlimited-timeouts <enabled>', 'Whether projects may set a timeout of 0, i.e. no timeout (true/false)')
    .option('--console-force-https <mode>', 'Redirect the console host to HTTPS: auto (once a cert exists), always, or never')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(updateSettingsAction)

  // Parity for GET /api/geo/status. Its own subcommand rather than more
  // output on `settings show`, because it reports the *live* freshness of the
  // loaded database (build epoch of what is answering lookups right now),
  // which is what an operator checks when a country column looks wrong.
  settings
    .command('geo-status')
    .description('Show the freshness of the geolocation (GeoLite2) database')
    .option('--json', 'Output in JSON format')
    .action(showGeoStatus)

  settings
    .command('set-external-url')
    .description('Set the external URL for the platform')
    .requiredOption('--url <url>', 'External URL')
    .action(setExternalUrl)

  settings
    .command('set-preview-domain')
    .description('Set the preview domain pattern')
    .requiredOption('--domain <domain>', 'Preview domain pattern')
    .action(setPreviewDomain)
}

async function showGeoStatus(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const status = await withSpinner('Fetching geolocation database status...', async () => {
    const { data, error } = await getGeoDatabaseStatus({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!status) {
    warning('The server did not report a geolocation database status')
    return
  }

  if (options.json) {
    json(status)
    return
  }

  const state = describeGeoRefreshState(status)

  newline()
  header(`${icons.info} Geolocation Database`)
  if (state.level === 'warn') {
    warning(state.headline)
  } else {
    success(state.headline)
  }
  info(state.detail)
  newline()

  keyValue('Source', status.source || colors.muted('Unknown (never downloaded here)'))
  keyValue(
    'MaxMind License Key',
    status.license_key_configured
      ? colors.success('Configured')
      : colors.muted('Not set'),
  )
  keyValue('Data Built', status.build_time || colors.muted('Unknown'))
  keyValue(
    'Data Age',
    status.age_days === null || status.age_days === undefined
      ? colors.muted('Unknown')
      : `${status.age_days} days`,
  )
  keyValue('Stale After', `${status.stale_after_days} days`)
  keyValue('Refresh Interval', `${status.refresh_interval_hours} hours`)
  keyValue('Last Refreshed', status.last_refreshed_at || colors.muted('Never'))
  keyValue('Last Check', status.last_check_at || colors.muted('Never run'))
  keyValue('Last Check Status', status.last_check_status || colors.muted('None recorded'))
  if (status.last_error) {
    keyValue('Last Error', colors.warning(status.last_error))
  }
}

async function showSettings(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const appSettings = await withSpinner('Fetching settings...', async () => {
    const { data, error } = await getSettings({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!appSettings) {
    warning('Settings not found')
    return
  }

  if (options.json) {
    json(appSettings)
    return
  }

  newline()
  header(`${icons.info} Platform Settings`)

  // General settings
  keyValue('External URL', appSettings.external_url || colors.muted('Not set'))
  keyValue(
    'Console HTTPS Redirect',
    appSettings.console_force_https === true
      ? 'Always'
      : appSettings.console_force_https === false
        ? colors.muted('Never')
        : colors.muted('Automatic (once a certificate exists)'),
  )
  keyValue('Preview Domain', appSettings.preview_domain || colors.muted('Not set'))

  // Let's Encrypt settings
  newline()
  header('Let\'s Encrypt')
  if (appSettings.letsencrypt) {
    keyValue('Email', appSettings.letsencrypt.email || colors.muted('Not set'))
    keyValue('Environment', appSettings.letsencrypt.environment || 'staging')
  } else {
    info('Not configured')
  }

  // DNS Provider settings
  newline()
  header('DNS Provider')
  if (appSettings.dns_provider && appSettings.dns_provider.provider) {
    keyValue('Provider', appSettings.dns_provider.provider)
    keyValue('API Key', appSettings.dns_provider.cloudflare_api_key || colors.muted('***'))
  } else {
    info('Not configured')
  }

  // Docker Registry settings
  newline()
  header('Docker Registry')
  if (appSettings.docker_registry && appSettings.docker_registry.registry_url) {
    keyValue('URL', appSettings.docker_registry.registry_url)
    keyValue('Username', appSettings.docker_registry.username || colors.muted('Not set'))
  } else {
    info('Not configured')
  }

  // Rate limiting settings
  newline()
  header('Rate Limiting')
  if (appSettings.rate_limiting) {
    keyValue('Enabled', appSettings.rate_limiting.enabled ? colors.success('Yes') : colors.muted('No'))
    if (appSettings.rate_limiting.enabled) {
      keyValue('Max Requests Per Minute', appSettings.rate_limiting.max_requests_per_minute?.toString() || '-')
    }
  } else {
    info('Not configured')
  }

  // Security headers
  newline()
  header('Security Headers')
  if (appSettings.security_headers) {
    keyValue('Enabled', appSettings.security_headers.enabled ? colors.success('Yes') : colors.muted('No'))
    keyValue('HSTS', appSettings.security_headers.strict_transport_security || colors.muted('Not set'))
    keyValue('XSS Protection', appSettings.security_headers.x_xss_protection || colors.muted('Not set'))
    keyValue('Content Type Options', appSettings.security_headers.x_content_type_options || colors.muted('Not set'))
    keyValue('Frame Options', appSettings.security_headers.x_frame_options || colors.muted('Not set'))
  } else {
    info('Not configured')
  }

  // Screenshots
  newline()
  header('Screenshots')
  if (appSettings.screenshots) {
    keyValue('Enabled', appSettings.screenshots.enabled ? colors.success('Yes') : colors.muted('No'))
  } else {
    info('Not configured')
  }

  // Geolocation. Shown unconditionally, and with the *effective* values, so
  // an unconfigured instance reports the defaults it is actually applying
  // rather than blanks an operator has to go read the source to interpret.
  newline()
  header('Geolocation Database')
  const geo = appSettings.geo
  keyValue(
    'MaxMind License Key',
    geo?.maxmind_license_key_saved
      ? colors.success('Configured')
      : colors.muted('Not set (using the bundled database)'),
  )
  keyValue('Refresh Interval', `${geo?.effective_refresh_interval_hours ?? 24} hours`)
  keyValue('Stored Lookup Lifetime', `${geo?.effective_stale_lookup_days ?? 30} days`)
  keyValue('Source', geo?.source || colors.muted('Unknown (never downloaded here)'))
  keyValue('Last Refreshed', geo?.last_refreshed_at || colors.muted('Never'))
  if (geo?.last_check_status === 'error') {
    keyValue(
      'Last Check',
      colors.warning(`Failed: ${geo.last_error ?? 'no reason recorded'}`),
    )
  } else if (geo?.last_check_status === 'skipped_no_license_key') {
    // Never blank: without a key the scheduled job downloads nothing, and that
    // is a configuration state with a fix, not a failure to hide.
    keyValue(
      'Last Check',
      colors.warning('Skipped — no MaxMind license key, so nothing is downloaded'),
    )
  } else {
    keyValue('Last Check', geo?.last_check_at || colors.muted('Never run'))
  }

  // Ceilings on what a project/environment may set for itself. All three are
  // unenforced by default, which is worth showing explicitly — "0" here means
  // "no ceiling", the opposite of what "0" means in a project's own config.
  newline()
  header('Project Override Ceilings')
  const ceilings = appSettings.tenant_resource_ceilings
  keyValue(
    'Max Memory Limit',
    ceilings?.max_memory_limit_mb
      ? `${ceilings.max_memory_limit_mb} MB`
      : colors.muted('No ceiling'),
  )
  keyValue(
    'Max Concurrent Connections',
    ceilings?.max_concurrent_connections
      ? ceilings.max_concurrent_connections.toString()
      : colors.muted('No ceiling'),
  )
  keyValue(
    'Projects May Disable Timeouts',
    ceilings?.allow_unlimited_request_timeouts === false
      ? colors.muted('No')
      : colors.success('Yes'),
  )

  newline()
}

async function updateSettingsAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  // Get current settings
  const { data: currentSettings, error: getError } = await getSettings({ client })
  if (getError) {
    throw new Error(getErrorMessage(getError))
  }

  const updates: Partial<AppSettings> = {}

  // Check if automation mode (specific flags provided)
  const isAutomation = options.yes && (
    options.externalUrl ||
    options.previewDomain ||
    options.letsencryptEmail ||
    options.letsencryptMode ||
    options.rateLimitingEnabled ||
    options.screenshotsEnabled ||
    options.maxRequestTimeout ||
    options.defaultHttpTimeout ||
    options.defaultSseIdleTimeout ||
    options.defaultWebsocketIdleTimeout ||
    options.maxMemoryLimitMb ||
    options.maxConcurrentConnectionsCeiling ||
    options.allowUnlimitedTimeouts ||
    options.consoleForceHttps ||
    (options.setting && options.value)
  )

  if (isAutomation) {
    const result = buildAutomationSettingsUpdate(options, currentSettings ?? undefined)
    if ('error' in result) {
      warning(result.error)
      return
    }
    Object.assign(updates, result.updates)
  } else {
    // Interactive mode
    const settingToUpdate = await promptSelect({
      message: 'Which setting would you like to update?',
      choices: [
        { name: 'External URL', value: 'external_url' },
        { name: 'Preview Domain', value: 'preview_domain' },
        { name: 'Let\'s Encrypt Settings', value: 'letsencrypt' },
        { name: 'Rate Limiting', value: 'rate_limiting' },
        { name: 'Security Headers', value: 'security_headers' },
        { name: 'Screenshots', value: 'screenshots' },
        { name: 'Request Timeouts', value: 'request_timeouts' },
        { name: 'Geolocation Database', value: 'geo' },
      ],
    })

    switch (settingToUpdate) {
      case 'external_url': {
        const url = await promptText({
          message: 'External URL',
          default: currentSettings?.external_url || '',
          required: true,
        })
        updates.external_url = url
        break
      }

      case 'preview_domain': {
        info('The preview domain pattern uses {{slug}} as a placeholder for the project slug.')
        info('Example: {{slug}}.preview.example.com')
        newline()
        const domain = await promptText({
          message: 'Preview domain pattern',
          default: currentSettings?.preview_domain || '',
          required: true,
        })
        updates.preview_domain = domain
        break
      }

      case 'letsencrypt': {
        const email = await promptText({
          message: 'Email for Let\'s Encrypt notifications',
          default: currentSettings?.letsencrypt?.email || '',
          required: true,
        })
        const environment = await promptSelect({
          message: 'Let\'s Encrypt environment',
          choices: [
            { name: 'Staging (for testing)', value: 'staging' },
            { name: 'Production', value: 'production' },
          ],
        })
        updates.letsencrypt = {
          email,
          environment,
        }
        break
      }

      case 'rate_limiting': {
        const enabled = await promptConfirm({
          message: 'Enable rate limiting?',
          default: currentSettings?.rate_limiting?.enabled ?? false,
        })

        let maxRequestsPerMinute = currentSettings?.rate_limiting?.max_requests_per_minute || 60
        if (enabled) {
          const rpmStr = await promptText({
            message: 'Max requests per minute',
            default: maxRequestsPerMinute.toString(),
            required: true,
          })
          maxRequestsPerMinute = parseInt(rpmStr, 10)
        }

        updates.rate_limiting = {
          enabled,
          max_requests_per_minute: maxRequestsPerMinute,
        }
        break
      }

      case 'security_headers': {
        const enabledHeaders = await promptConfirm({
          message: 'Enable security headers?',
          default: currentSettings?.security_headers?.enabled ?? true,
        })

        updates.security_headers = {
          enabled: enabledHeaders,
          strict_transport_security: currentSettings?.security_headers?.strict_transport_security || 'max-age=31536000; includeSubDomains',
          x_xss_protection: currentSettings?.security_headers?.x_xss_protection || '1; mode=block',
          x_content_type_options: currentSettings?.security_headers?.x_content_type_options || 'nosniff',
          x_frame_options: currentSettings?.security_headers?.x_frame_options || 'DENY',
        }
        break
      }

      case 'screenshots': {
        const enabled = await promptConfirm({
          message: 'Enable automatic screenshots for deployments?',
          default: currentSettings?.screenshots?.enabled ?? false,
        })

        updates.screenshots = {
          enabled,
        }
        break
      }

      case 'request_timeouts': {
        info('0 means no timeout. Timeouts are opt-in — existing apps are unaffected until you set a nonzero default here.')
        info('The hard ceiling only applies once a timeout is actually configured; it never creates one on its own.')
        newline()

        const maxRequestTimeout = await promptNumber(
          'Hard ceiling for all request/idle timeouts (seconds)',
          { default: currentSettings?.request_timeouts?.max_request_timeout_seconds ?? 600, min: 5 }
        )
        const defaultHttpTimeout = await promptNumber(
          'Default timeout for regular HTTP requests (seconds, 0 = no timeout)',
          { default: currentSettings?.request_timeouts?.default_http_timeout_seconds ?? 0, min: 0 }
        )
        const defaultSseIdleTimeout = await promptNumber(
          'Default idle timeout for SSE streams (seconds, 0 = no timeout)',
          { default: currentSettings?.request_timeouts?.default_sse_idle_timeout_seconds ?? 0, min: 0 }
        )
        const defaultWebsocketIdleTimeout = await promptNumber(
          'Default idle timeout for WebSocket connections (seconds, 0 = no timeout)',
          { default: currentSettings?.request_timeouts?.default_websocket_idle_timeout_seconds ?? 0, min: 0 }
        )

        updates.request_timeouts = {
          max_request_timeout_seconds: maxRequestTimeout,
          default_http_timeout_seconds: defaultHttpTimeout,
          default_sse_idle_timeout_seconds: defaultSseIdleTimeout,
          default_websocket_idle_timeout_seconds: defaultWebsocketIdleTimeout,
        }
        break
      }

      case 'geo': {
        const currentGeo = currentSettings?.geo

        info('Temps re-downloads the MaxMind GeoLite2 city database on a schedule so country/city on proxy logs, analytics and audit entries stay correct.')
        info(
          currentGeo?.maxmind_license_key_saved
            ? 'A MaxMind license key is configured; downloads use MaxMind directly.'
            : 'No MaxMind license key is configured; downloads fall back to the copy bundled with the repository.'
        )
        if (currentGeo?.last_check_status === 'error') {
          warning(`Last refresh check failed: ${currentGeo.last_error ?? 'no reason recorded'}`)
        }
        newline()

        const refreshIntervalHours = await promptNumber(
          'Refresh interval (hours)',
          {
            default: currentGeo?.effective_refresh_interval_hours ?? 24,
            min: 1,
            max: 8760,
          }
        )
        const staleLookupDays = await promptNumber(
          'Re-resolve a stored IP lookup after (days)',
          {
            default: currentGeo?.effective_stale_lookup_days ?? 30,
            min: 1,
            max: 3650,
          }
        )
        // Blank preserves the stored key, matching the server's contract and
        // the console UI. Read with promptPassword so it is not echoed.
        const licenseKey = (
          await promptPassword({
            message: 'MaxMind license key (leave blank to keep the current one)',
          })
        ).trim()

        // Sent as a write-only field the server encrypts before storing; the
        // GET response only ever reports `maxmind_license_key_saved`.
        const geoUpdate: Record<string, unknown> = {
          ...(currentGeo ?? {}),
          refresh_interval_hours: refreshIntervalHours,
          stale_lookup_days: staleLookupDays,
        }
        if (licenseKey) {
          geoUpdate.maxmind_license_key = licenseKey
        }
        ;(updates as unknown as Record<string, unknown>).geo = geoUpdate
        break
      }
    }
  }

  await withSpinner('Updating settings...', async () => {
    // The server's PUT /settings deserializes the whole body straight into
    // `AppSettings`, whose fields are `#[serde(default)]` — so any field
    // omitted from this request is indistinguishable from "explicitly reset
    // to default" and gets wiped server-side (masked/sensitive fields like
    // credentials are the only ones the server restores automatically).
    // Sending only `updates` would silently reset every untouched setting
    // (rate limiting, security headers, request timeouts, monitoring, etc.)
    // back to its Rust default. Merge onto the settings already fetched
    // above so a change to one setting can never clobber another.
    const { error } = await updateSettings({
      client,
      body: { ...currentSettings, ...updates } as AppSettings,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Settings updated successfully')
}

async function setExternalUrl(options: SetExternalUrlOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  await withSpinner('Updating external URL...', async () => {
    // See the comment in updateSettingsAction: PUT /settings replaces every
    // field not present in the body with its default, so this must send the
    // full current settings with only external_url changed.
    const { data: currentSettings, error: getError } = await getSettings({ client })
    if (getError) {
      throw new Error(getErrorMessage(getError))
    }
    const { error } = await updateSettings({
      client,
      body: { ...currentSettings, external_url: options.url } as AppSettings,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`External URL set to: ${options.url}`)
}

async function setPreviewDomain(options: SetPreviewDomainOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  await withSpinner('Updating preview domain...', async () => {
    // See the comment in updateSettingsAction: PUT /settings replaces every
    // field not present in the body with its default, so this must send the
    // full current settings with only preview_domain changed.
    const { data: currentSettings, error: getError } = await getSettings({ client })
    if (getError) {
      throw new Error(getErrorMessage(getError))
    }
    const { error } = await updateSettings({
      client,
      body: { ...currentSettings, preview_domain: options.domain } as AppSettings,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Preview domain set to: ${options.domain}`)
}
