import type { Command } from 'commander'
import { config, credentials, type TempsConfig } from '../config/store.js'
import { promptUrl, promptSelect, promptConfirm, promptPassword } from '../ui/prompts.js'
import { success, info, colors, newline, header, keyValue, icons, box } from '../ui/output.js'
import { shouldBeInteractive } from '../utils/tty.js'
import { setupClient, client } from '../lib/api-client.js'
import { getCurrentUser } from '../api/sdk.gen.js'

interface ConfigureOptions {
  apiUrl?: string
  apiToken?: string
  outputFormat?: 'table' | 'json' | 'minimal'
  enableColors?: boolean
  interactive?: boolean  // --no-interactive sets this to false
}

export function registerConfigureCommand(program: Command): void {
  const configure = program
    .command('configure')
    .description('Configure CLI settings (AWS-style wizard)')
    .option('--api-url <url>', 'API URL')
    .option('--api-token <token>', 'API token for authentication')
    .option('--output-format <format>', 'Output format (table, json, minimal)')
    .option('--enable-colors', 'Enable colored output in config')
    .option('--disable-colors', 'Disable colored output in config')
    .option('-i, --interactive', 'Force interactive mode even in non-TTY')
    .option('-y, --no-interactive', 'Non-interactive mode (uses defaults for unspecified options)')
    .action(runConfigureWizard)

  configure
    .command('get <key>')
    .description('Get a configuration value')
    .action(getConfigValue)

  configure
    .command('set <key> <value>')
    .description('Set a configuration value')
    .action(setConfigValue)

  configure
    .command('list')
    .description('List all configuration values')
    .action(listConfig)

  configure
    .command('reset')
    .description('Reset configuration to defaults')
    .action(resetConfig)
}

async function runConfigureWizard(options: ConfigureOptions & { disableColors?: boolean }): Promise<void> {
  const currentConfig = config.getAll()
  const isAuthenticated = await credentials.isAuthenticated()
  const currentEmail = await credentials.get('email')

  // Use shouldBeInteractive utility for TTY detection with explicit flag override
  const isInteractive = shouldBeInteractive(options.interactive)

  // Determine color setting from flags
  const colorFromFlags = options.enableColors === true ? true :
                         options.disableColors === true ? false : undefined

  // Non-interactive mode: use provided flags or current values
  if (!isInteractive) {
    const apiUrl = options.apiUrl ?? currentConfig.apiUrl
    const outputFormat = options.outputFormat ?? currentConfig.outputFormat
    const colorEnabled = colorFromFlags ?? currentConfig.colorEnabled

    // Validate outputFormat
    if (options.outputFormat && !['table', 'json', 'minimal'].includes(options.outputFormat)) {
      console.error(colors.error(`Invalid output format: ${options.outputFormat}`))
      console.error(colors.muted('Valid formats: table, json, minimal'))
      process.exit(1)
    }

    config.setAll({ apiUrl, outputFormat, colorEnabled })

    // Handle API token if provided
    if (options.apiToken) {
      const tokenValid = await validateAndSaveToken(options.apiToken, apiUrl)
      if (!tokenValid) {
        process.exit(1)
      }
    }

    const authStatus = await credentials.isAuthenticated()
      ? `Authenticated as ${await credentials.get('email') ?? 'unknown'}`
      : 'Not authenticated'

    newline()
    box(
      `API URL: ${apiUrl}\n` +
        `API Token: ${authStatus}\n` +
        `Output Format: ${outputFormat}\n` +
        `Colors: ${colorEnabled ? 'enabled' : 'disabled'}`,
      `${icons.check} Configuration saved`
    )
    return
  }

  // Interactive mode
  newline()
  console.log(colors.bold(`${icons.sparkles} Temps CLI Configuration`))
  console.log(colors.muted('This wizard will help you configure the CLI.\n'))

  // API URL (skip prompt if provided via flag)
  const apiUrl = options.apiUrl ?? await promptUrl(
    `API URL [${colors.muted(currentConfig.apiUrl)}]`,
    currentConfig.apiUrl
  )

  // Save API URL first (needed for token validation)
  config.set('apiUrl', apiUrl)

  // API Token configuration
  let authStatus = 'Not authenticated'
  if (options.apiToken) {
    // Token provided via flag
    const tokenValid = await validateAndSaveToken(options.apiToken, apiUrl)
    if (tokenValid) {
      authStatus = `Authenticated as ${await credentials.get('email') ?? 'unknown'}`
    }
  } else if (isAuthenticated) {
    // Already authenticated, ask if they want to update
    console.log(colors.muted(`\nCurrently authenticated as: ${colors.bold(currentEmail ?? 'unknown')}`))
    const updateToken = await promptConfirm({
      message: 'Update API token?',
      default: false,
    })
    if (updateToken) {
      const newToken = await promptPassword({
        message: 'API Token',
        validate: (value) => {
          if (!value || value.trim().length === 0) {
            return 'API token is required'
          }
          return true
        },
      })
      const tokenValid = await validateAndSaveToken(newToken, apiUrl)
      if (tokenValid) {
        authStatus = `Authenticated as ${await credentials.get('email') ?? 'unknown'}`
      }
    } else {
      authStatus = `Authenticated as ${currentEmail ?? 'unknown'}`
    }
  } else {
    // Not authenticated, prompt for token
    const configureToken = await promptConfirm({
      message: 'Configure API token now?',
      default: true,
    })
    if (configureToken) {
      const newToken = await promptPassword({
        message: 'API Token',
        validate: (value) => {
          if (!value || value.trim().length === 0) {
            return 'API token is required'
          }
          return true
        },
      })
      const tokenValid = await validateAndSaveToken(newToken, apiUrl)
      if (tokenValid) {
        authStatus = `Authenticated as ${await credentials.get('email') ?? 'unknown'}`
      }
    }
  }

  // Output format (skip prompt if provided via flag)
  const outputFormat = options.outputFormat ?? await promptSelect<'table' | 'json' | 'minimal'>({
    message: 'Default output format',
    choices: [
      { name: 'Table', value: 'table', description: 'Formatted tables (default)' },
      { name: 'JSON', value: 'json', description: 'Raw JSON output' },
      { name: 'Minimal', value: 'minimal', description: 'Compact output' },
    ],
    default: currentConfig.outputFormat,
  })

  // Color (skip prompt if provided via flag)
  const colorEnabled = colorFromFlags ?? await promptConfirm({
    message: 'Enable colored output?',
    default: currentConfig.colorEnabled,
  })

  // Save configuration
  config.setAll({
    apiUrl,
    outputFormat,
    colorEnabled,
  })

  newline()
  box(
    `API URL: ${apiUrl}\n` +
      `API Token: ${authStatus}\n` +
      `Output Format: ${outputFormat}\n` +
      `Colors: ${colorEnabled ? 'enabled' : 'disabled'}`,
    `${icons.check} Configuration saved`
  )

  newline()
  info(`Configuration file: ${colors.muted(config.path)}`)
  info(`Credentials file: ${colors.muted(credentials.path)}`)
}

function getConfigValue(key: string): void {
  const validKeys: (keyof TempsConfig)[] = [
    'apiUrl',
    'defaultProject',
    'defaultEnvironment',
    'outputFormat',
    'colorEnabled',
  ]

  if (!validKeys.includes(key as keyof TempsConfig)) {
    console.error(colors.error(`Unknown configuration key: ${key}`))
    console.error(colors.muted(`Valid keys: ${validKeys.join(', ')}`))
    process.exit(1)
  }

  const value = config.get(key as keyof TempsConfig)
  console.log(value ?? '')
}

function setConfigValue(key: string, value: string): void {
  const validKeys: (keyof TempsConfig)[] = [
    'apiUrl',
    'defaultProject',
    'defaultEnvironment',
    'outputFormat',
    'colorEnabled',
  ]

  if (!validKeys.includes(key as keyof TempsConfig)) {
    console.error(colors.error(`Unknown configuration key: ${key}`))
    console.error(colors.muted(`Valid keys: ${validKeys.join(', ')}`))
    process.exit(1)
  }

  // Type conversion
  let typedValue: string | boolean = value
  if (key === 'colorEnabled') {
    typedValue = value === 'true' || value === '1'
  }

  config.set(key as keyof TempsConfig, typedValue as never)
  success(`Set ${key} = ${value}`)
}

function listConfig(): void {
  const allConfig = config.getAll()

  newline()
  header(`${icons.folder} Configuration`)

  for (const [key, value] of Object.entries(allConfig)) {
    keyValue(key, value)
  }

  newline()
  info(`Configuration file: ${colors.muted(config.path)}`)
}

async function resetConfig(): Promise<void> {
  const confirmed = await promptConfirm({
    message: 'Are you sure you want to reset configuration to defaults?',
    default: false,
  })

  if (!confirmed) {
    info('Cancelled')
    return
  }

  config.reset()
  success('Configuration reset to defaults')
}

/**
 * Validate an API token and save credentials if valid
 */
async function validateAndSaveToken(apiToken: string, apiUrl: string): Promise<boolean> {
  // Temporarily set the API key to validate it
  await credentials.set('apiKey', apiToken)

  // Setup client with the new credentials
  await setupClient()

  try {
    console.log(colors.muted('Validating API token...'))
    const { data, error } = await getCurrentUser({ client })

    if (error || !data) {
      console.error(colors.error('Invalid API token'))
      await credentials.clear()
      return false
    }

    await credentials.setAll({
      apiKey: apiToken,
      userId: data.id,
      email: data.email ?? undefined,
    })

    console.log(colors.success(`${icons.check} API token validated`))
    return true
  } catch (err) {
    console.error(colors.error('Failed to validate API token'))
    await credentials.clear()
    return false
  }
}
