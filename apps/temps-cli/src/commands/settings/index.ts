import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getSettings,
  updateSettings,
} from '../../api/sdk.gen.js'
import type { AppSettings } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptText, promptConfirm, promptSelect, promptNumber } from '../../ui/prompts.js'
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
interface CurrentSettingsSnapshot {
  letsencrypt?: { email?: string | null; environment?: string } | null
  rate_limiting?: { max_requests_per_minute?: number } | null
  request_timeouts?: {
    max_request_timeout_seconds?: number
    default_http_timeout_seconds?: number
    default_sse_idle_timeout_seconds?: number
    default_websocket_idle_timeout_seconds?: number
  } | null
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
    .option('--console-force-https <mode>', 'Redirect the console host to HTTPS: auto (once a cert exists), always, or never')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(updateSettingsAction)

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
