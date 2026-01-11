import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getSettings,
  updateSettings,
} from '../../api/sdk.gen.js'
import type { AppSettings } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptText, promptConfirm, promptSelect } from '../../ui/prompts.js'
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
  yes?: boolean
}

interface SetExternalUrlOptions {
  url: string
}

interface SetPreviewDomainOptions {
  domain: string
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
    (options.setting && options.value)
  )

  if (isAutomation) {
    // Handle specific flags
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
    if (options.screenshotsEnabled !== undefined) {
      const enabled = options.screenshotsEnabled === 'true'
      updates.screenshots = {
        enabled,
      }
    }

    // Handle generic setting/value pair
    if (options.setting && options.value) {
      switch (options.setting) {
        case 'external_url':
          updates.external_url = options.value
          break
        case 'preview_domain':
          updates.preview_domain = options.value
          break
        default:
          warning(`Unknown setting: ${options.setting}`)
          return
      }
    }

    if (Object.keys(updates).length === 0) {
      warning('No settings to update')
      return
    }
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
    }
  }

  await withSpinner('Updating settings...', async () => {
    const { error } = await updateSettings({
      client,
      body: updates as AppSettings,
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
    const { error } = await updateSettings({
      client,
      body: {
        external_url: options.url,
      } as AppSettings,
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
    const { error } = await updateSettings({
      client,
      body: {
        preview_domain: options.domain,
      } as AppSettings,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Preview domain set to: ${options.domain}`)
}
