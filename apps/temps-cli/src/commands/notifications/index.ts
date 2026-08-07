import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listNotificationProviders,
  createSlackProvider,
  createNotificationProvider,
  getNotificationProvider,
  deleteNotificationProvider as deleteProvider2,
  testNotificationProvider as testProvider2,
  updateNotificationProvider as updateProvider2,
  updateSlackProvider,
  updateNotificationEmailProvider,
} from '../../api/sdk.gen.js'
import type { NotificationProviderResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm, promptNumber } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface AddOptions {
  type?: string
  name?: string
  webhookUrl?: string
  channel?: string
  // Email SMTP options
  smtpHost?: string
  smtpPort?: string
  username?: string
  password?: string
  fromAddress?: string
  fromName?: string
  toAddresses?: string
  // Webhook options
  url?: string
  method?: string
  yes?: boolean
}

interface UpdateOptions {
  id: string
  name?: string
  enabled?: string
  // Slack options
  webhookUrl?: string
  channel?: string
  // Email SMTP options
  smtpHost?: string
  smtpPort?: string
  username?: string
  password?: string
  fromAddress?: string
  fromName?: string
  toAddresses?: string
  // Webhook options
  url?: string
  method?: string
  json?: boolean
  yes?: boolean
}

interface ShowOptions {
  id: string
  json?: boolean
}

interface RemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

interface TestOptions {
  id: string
}

interface EnableDisableOptions {
  id: string
  json?: boolean
}

export function registerNotificationsCommands(program: Command): void {
  const notifications = program
    .command('notifications')
    .alias('notify')
    .description('Manage notification providers (Slack, Email, Webhook, etc.)')

  notifications
    .command('list')
    .alias('ls')
    .description('List configured notification providers')
    .option('--json', 'Output in JSON format')
    .action(listProviders)

  notifications
    .command('add')
    .description('Add a new notification provider')
    .option('-t, --type <type>', 'Provider type (slack, email, webhook)')
    .option('-n, --name <name>', 'Provider name')
    .option('-w, --webhook-url <url>', 'Webhook URL (for slack)')
    .option('-c, --channel <channel>', 'Channel name (for slack, optional)')
    .option('--smtp-host <host>', 'SMTP host (for email)')
    .option('--smtp-port <port>', 'SMTP port (for email)')
    .option('--username <username>', 'SMTP username (for email)')
    .option('--password <password>', 'SMTP password (for email)')
    .option('--from-address <address>', 'From email address (for email)')
    .option('--from-name <name>', 'From display name (for email, optional)')
    .option('--to-addresses <addresses>', 'Comma-separated recipient addresses (for email)')
    .option('--url <url>', 'Webhook URL (for webhook)')
    .option('--method <method>', 'HTTP method: POST, PUT, PATCH (for webhook, default: POST)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(addProvider)

  notifications
    .command('update')
    .description('Update a notification provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-n, --name <name>', 'New provider name')
    .option('--enabled <enabled>', 'Enable or disable (true/false)')
    .option('-w, --webhook-url <url>', 'Webhook URL (for slack)')
    .option('-c, --channel <channel>', 'Channel name (for slack)')
    .option('--smtp-host <host>', 'SMTP host (for email)')
    .option('--smtp-port <port>', 'SMTP port (for email)')
    .option('--username <username>', 'SMTP username (for email)')
    .option('--password <password>', 'SMTP password (for email)')
    .option('--from-address <address>', 'From email address (for email)')
    .option('--from-name <name>', 'From display name (for email)')
    .option('--to-addresses <addresses>', 'Comma-separated recipient addresses (for email)')
    .option('--url <url>', 'Webhook URL (for webhook)')
    .option('--method <method>', 'HTTP method: POST, PUT, PATCH (for webhook)')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip confirmation prompts')
    .action(updateProvider)

  notifications
    .command('enable')
    .description('Enable a notification provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(enableProvider)

  notifications
    .command('disable')
    .description('Disable a notification provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(disableProvider)

  notifications
    .command('show')
    .description('Show notification provider details')
    .requiredOption('--id <id>', 'Provider ID')
    .option('--json', 'Output in JSON format')
    .action(showProvider)

  notifications
    .command('remove')
    .alias('rm')
    .description('Remove a notification provider')
    .requiredOption('--id <id>', 'Provider ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeProvider)

  notifications
    .command('test')
    .description('Send a test notification')
    .requiredOption('--id <id>', 'Provider ID')
    .action(testProviderAction)
}

async function listProviders(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const providers = await withSpinner('Fetching notification providers...', async () => {
    const { data, error } = await listNotificationProviders({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(providers)
    return
  }

  newline()
  header(`${icons.info} Notification Providers (${providers.length})`)

  if (providers.length === 0) {
    info('No notification providers configured')
    info('Run: temps notifications add --type slack --name my-slack --webhook-url <url> -y')
    newline()
    return
  }

  const columns: TableColumn<NotificationProviderResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Type', key: 'provider_type' },
    { header: 'Status', accessor: (p) => p.enabled ? 'enabled' : 'disabled', color: (v) => statusBadge(v === 'enabled' ? 'active' : 'inactive') },
  ]

  printTable(providers, columns, { style: 'minimal' })
  newline()
}

async function addProvider(options: AddOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const providerType = options.type || await promptSelect({
    message: 'Notification provider type',
    choices: [
      { name: 'Slack', value: 'slack' },
      { name: 'Email (SMTP)', value: 'email' },
      { name: 'Webhook', value: 'webhook' },
    ],
  })

  switch (providerType) {
    case 'slack':
      await addSlackProvider(options)
      break
    case 'email':
      await addEmailProvider(options)
      break
    case 'webhook':
      await addWebhookProvider(options)
      break
    default:
      warning(`Invalid provider type: ${providerType}. Supported: slack, email, webhook`)
  }
}

async function addSlackProvider(options: AddOptions): Promise<void> {
  let name: string
  let webhookUrl: string
  let channel: string | null = null

  const isAutomation = options.yes && options.name && options.webhookUrl

  if (isAutomation) {
    name = options.name!
    webhookUrl = options.webhookUrl!
    channel = options.channel || null
  } else {
    name = options.name || await promptText({
      message: 'Provider name',
      default: 'slack-notifications',
      required: true,
    })

    info('\nYou need a Slack Incoming Webhook URL.')
    info('Create one at: https://api.slack.com/messaging/webhooks')
    newline()

    webhookUrl = options.webhookUrl || await promptPassword({
      message: 'Webhook URL',
    })

    channel = options.channel || await promptText({
      message: 'Channel name (optional)',
      default: '',
    }) || null
  }

  await withSpinner('Creating Slack notification provider...', async () => {
    const { error } = await createSlackProvider({
      client,
      body: {
        name,
        config: {
          webhook_url: webhookUrl,
          channel,
        },
        enabled: true,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Slack notification provider created successfully')
  info('Run: temps notifications test --id <id> to send a test notification')
}

async function addEmailProvider(options: AddOptions): Promise<void> {
  let name: string
  let smtpHost: string
  let smtpPort: number
  let username: string
  let smtpPassword: string
  let fromAddress: string
  let fromName: string | null = null
  let toAddresses: string[]

  const isAutomation = options.yes && options.name && options.smtpHost && options.smtpPort
    && options.username && options.password && options.fromAddress && options.toAddresses

  if (isAutomation) {
    name = options.name!
    smtpHost = options.smtpHost!
    smtpPort = parseInt(options.smtpPort!, 10)
    username = options.username!
    smtpPassword = options.password!
    fromAddress = options.fromAddress!
    fromName = options.fromName || null
    toAddresses = options.toAddresses!.split(',').map((a) => a.trim())
  } else {
    name = options.name || await promptText({
      message: 'Provider name',
      default: 'email-notifications',
      required: true,
    })

    smtpHost = options.smtpHost || await promptText({
      message: 'SMTP host',
      required: true,
    })

    smtpPort = options.smtpPort
      ? parseInt(options.smtpPort, 10)
      : await promptNumber('SMTP port', { default: 587, min: 1, max: 65535 })

    username = options.username || await promptText({
      message: 'SMTP username',
      required: true,
    })

    smtpPassword = options.password || await promptPassword({
      message: 'SMTP password',
    })

    fromAddress = options.fromAddress || await promptText({
      message: 'From email address',
      required: true,
    })

    const fromNameInput = options.fromName ?? await promptText({
      message: 'From display name (optional)',
      default: '',
    })
    fromName = fromNameInput || null

    const toAddressesInput = options.toAddresses || await promptText({
      message: 'Recipient email addresses (comma-separated)',
      required: true,
    })
    toAddresses = toAddressesInput.split(',').map((a) => a.trim())
  }

  await withSpinner('Creating Email notification provider...', async () => {
    const { error } = await createNotificationProvider({
      client,
      body: {
        name,
        provider_type: 'email',
        config: {
          smtp_host: smtpHost,
          smtp_port: smtpPort,
          username,
          password: smtpPassword,
          from_address: fromAddress,
          from_name: fromName,
          to_addresses: toAddresses,
        },
        enabled: true,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Email notification provider created successfully')
  info('Run: temps notifications test --id <id> to send a test notification')
}

async function addWebhookProvider(options: AddOptions): Promise<void> {
  let name: string
  let url: string
  let method: string

  const isAutomation = options.yes && options.name && options.url

  if (isAutomation) {
    name = options.name!
    url = options.url!
    method = options.method || 'POST'
  } else {
    name = options.name || await promptText({
      message: 'Provider name',
      default: 'webhook-notifications',
      required: true,
    })

    url = options.url || await promptText({
      message: 'Webhook URL',
      required: true,
    })

    method = options.method || await promptSelect({
      message: 'HTTP method',
      choices: [
        { name: 'POST', value: 'POST' },
        { name: 'PUT', value: 'PUT' },
        { name: 'PATCH', value: 'PATCH' },
      ],
    })
  }

  await withSpinner('Creating Webhook notification provider...', async () => {
    const { error } = await createNotificationProvider({
      client,
      body: {
        name,
        provider_type: 'webhook',
        config: {
          url,
          method,
          headers: {},
          timeout_secs: 30,
        },
        enabled: true,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Webhook notification provider created successfully')
  info('Run: temps notifications test --id <id> to send a test notification')
}

async function updateProvider(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  // Fetch current provider to detect type
  const provider = await withSpinner('Fetching provider...', async () => {
    const { data, error } = await getNotificationProvider({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Provider ${options.id} not found`)
    }
    return data
  })

  const providerType = provider.provider_type

  // Parse enabled flag if provided
  let enabled: boolean | undefined
  if (options.enabled !== undefined) {
    if (options.enabled === 'true') {
      enabled = true
    } else if (options.enabled === 'false') {
      enabled = false
    } else {
      warning('Invalid value for --enabled. Use "true" or "false"')
      return
    }
  }

  // Check if any type-specific config options were provided
  const hasSlackConfig = options.webhookUrl !== undefined || options.channel !== undefined
  const hasEmailConfig = options.smtpHost !== undefined || options.smtpPort !== undefined
    || options.username !== undefined || options.password !== undefined
    || options.fromAddress !== undefined || options.fromName !== undefined
    || options.toAddresses !== undefined
  const hasWebhookConfig = options.url !== undefined || options.method !== undefined

  // Route to type-specific update if config changes are provided
  if (providerType === 'slack' && hasSlackConfig) {
    await updateSlackProviderAction(id, provider, options, enabled)
  } else if (providerType === 'email' && hasEmailConfig) {
    await updateEmailProviderAction(id, provider, options, enabled)
  } else if (providerType === 'webhook' && hasWebhookConfig) {
    await updateWebhookProviderAction(id, provider, options, enabled)
  } else {
    // Generic update (name and/or enabled only)
    const updated = await withSpinner('Updating provider...', async () => {
      const { data, error } = await updateProvider2({
        client,
        path: { id },
        body: {
          name: options.name ?? undefined,
          enabled: enabled ?? undefined,
        },
      })
      if (error) {
        throw new Error(getErrorMessage(error))
      }
      return data
    })

    if (options.json && updated) {
      json(updated)
      return
    }

    success(`Provider "${updated?.name ?? provider.name}" updated successfully`)
  }
}

async function updateSlackProviderAction(
  id: number,
  provider: NotificationProviderResponse,
  options: UpdateOptions,
  enabled?: boolean,
): Promise<void> {
  const currentConfig = provider.config as { webhook_url?: string; channel?: string | null } | null

  const updated = await withSpinner('Updating Slack provider...', async () => {
    const { data, error } = await updateSlackProvider({
      client,
      path: { id },
      body: {
        name: options.name ?? undefined,
        enabled: enabled ?? undefined,
        config: {
          webhook_url: options.webhookUrl ?? currentConfig?.webhook_url ?? '',
          channel: options.channel !== undefined ? (options.channel || null) : (currentConfig?.channel ?? null),
        },
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json && updated) {
    json(updated)
    return
  }

  success(`Slack provider "${updated?.name ?? provider.name}" updated successfully`)
}

async function updateEmailProviderAction(
  id: number,
  provider: NotificationProviderResponse,
  options: UpdateOptions,
  enabled?: boolean,
): Promise<void> {
  const currentConfig = provider.config as {
    smtp_host?: string
    smtp_port?: number
    username?: string
    password?: string
    from_address?: string
    from_name?: string | null
    to_addresses?: string[]
  } | null

  const updated = await withSpinner('Updating Email provider...', async () => {
    const { data, error } = await updateNotificationEmailProvider({
      client,
      path: { id },
      body: {
        name: options.name ?? undefined,
        enabled: enabled ?? undefined,
        config: {
          smtp_host: options.smtpHost ?? currentConfig?.smtp_host ?? '',
          smtp_port: options.smtpPort ? parseInt(options.smtpPort, 10) : (currentConfig?.smtp_port ?? 587),
          username: options.username ?? currentConfig?.username ?? '',
          password: options.password ?? currentConfig?.password ?? '',
          from_address: options.fromAddress ?? currentConfig?.from_address ?? '',
          from_name: options.fromName !== undefined ? (options.fromName || null) : (currentConfig?.from_name ?? null),
          to_addresses: options.toAddresses
            ? options.toAddresses.split(',').map((a) => a.trim())
            : (currentConfig?.to_addresses ?? []),
        },
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json && updated) {
    json(updated)
    return
  }

  success(`Email provider "${updated?.name ?? provider.name}" updated successfully`)
}

async function updateWebhookProviderAction(
  id: number,
  provider: NotificationProviderResponse,
  options: UpdateOptions,
  enabled?: boolean,
): Promise<void> {
  const currentConfig = provider.config as {
    url?: string
    method?: string
    headers?: Record<string, string>
    timeout_secs?: number
  } | null

  const updated = await withSpinner('Updating Webhook provider...', async () => {
    const { data, error } = await updateProvider2({
      client,
      path: { id },
      body: {
        name: options.name ?? undefined,
        enabled: enabled ?? undefined,
        config: {
          url: options.url ?? currentConfig?.url ?? '',
          method: options.method ?? currentConfig?.method ?? 'POST',
          headers: currentConfig?.headers ?? {},
          timeout_secs: currentConfig?.timeout_secs ?? 30,
        },
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json && updated) {
    json(updated)
    return
  }

  success(`Webhook provider "${updated?.name ?? provider.name}" updated successfully`)
}

async function enableProvider(options: EnableDisableOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const updated = await withSpinner('Enabling provider...', async () => {
    const { data, error } = await updateProvider2({
      client,
      path: { id },
      body: {
        enabled: true,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json && updated) {
    json(updated)
    return
  }

  success(`Provider "${updated?.name}" enabled`)
}

async function disableProvider(options: EnableDisableOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const updated = await withSpinner('Disabling provider...', async () => {
    const { data, error } = await updateProvider2({
      client,
      path: { id },
      body: {
        enabled: false,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json && updated) {
    json(updated)
    return
  }

  success(`Provider "${updated?.name}" disabled`)
}

async function showProvider(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  const provider = await withSpinner('Fetching provider...', async () => {
    const { data, error } = await getNotificationProvider({
      client,
      path: { id },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Provider ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(provider)
    return
  }

  newline()
  header(`${icons.info} ${provider.name}`)
  keyValue('ID', provider.id)
  keyValue('Type', provider.provider_type)
  keyValue('Status', provider.enabled ? colors.success('enabled') : colors.muted('disabled'))
  keyValue('Created', new Date(provider.created_at * 1000).toLocaleString())
  keyValue('Updated', new Date(provider.updated_at * 1000).toLocaleString())
  newline()
}

async function removeProvider(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  // Get provider details first
  const { data: provider, error: getError } = await getNotificationProvider({
    client,
    path: { id },
  })

  if (getError || !provider) {
    warning(`Provider ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove notification provider "${provider.name}" (${provider.provider_type})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing provider...', async () => {
    const { error } = await deleteProvider2({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Notification provider removed')
}

async function testProviderAction(options: TestOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid provider ID')
    return
  }

  await withSpinner('Sending test notification...', async () => {
    const { error } = await testProvider2({
      client,
      path: { id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Test notification sent successfully!')
  info('Check your notification channel for the test message')
}
