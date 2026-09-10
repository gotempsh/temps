// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
  listNotificationRoutes,
  createNotificationRoute,
  getNotificationRoute,
  updateNotificationRoute,
  deleteNotificationRoute,
} from '../../api/sdk.gen.js'
import type { NotificationProviderResponse, NotificationRoute } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm, promptNumber, promptCheckbox } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

const SEVERITIES = ['debug', 'info', 'warning', 'error', 'critical', 'emergency'] as const

interface RouteListOptions {
  json?: boolean
}

interface RouteShowOptions {
  id: string
  json?: boolean
}

interface RouteCreateOptions {
  name?: string
  minSeverity?: string
  maxSeverity?: string
  providerIds?: string
  enabled?: string
  json?: boolean
  yes?: boolean
}

interface RouteUpdateOptions {
  id: string
  name?: string
  minSeverity?: string
  maxSeverity?: string
  providerIds?: string
  enabled?: string
  json?: boolean
}

interface RouteRemoveOptions {
  id: string
  force?: boolean
  yes?: boolean
}

export function parseProviderIds(value: string): number[] {
  return value
    .split(',')
    .map((id) => id.trim())
    .filter((id) => id.length > 0)
    .map((id) => parseInt(id, 10))
}

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

interface CurrentSlackConfig {
  webhook_url?: string
  channel?: string | null
}

interface SlackProviderConfig {
  webhook_url: string
  channel?: string | null
}

interface CurrentEmailConfig {
  smtp_host?: string
  smtp_port?: number
  username?: string
  password?: string
  from_address?: string
  from_name?: string | null
  to_addresses?: string[]
}

interface EmailProviderConfig {
  smtp_host: string
  smtp_port: number
  username: string
  password: string
  from_address: string
  from_name?: string | null
  to_addresses: string[]
}

interface WebhookProviderConfig {
  url?: string
  method?: string
  headers?: Record<string, string>
  timeout_secs?: number
}

/** Parses --enabled; returns 'invalid' rather than throwing so the caller keeps its warn-and-exit UX. */
export function parseEnabledFlag(value: string | undefined): boolean | undefined | 'invalid' {
  if (value === undefined) return undefined
  if (value === 'true') return true
  if (value === 'false') return false
  return 'invalid'
}

export function hasSlackConfigChanges(options: { webhookUrl?: string; channel?: string }): boolean {
  return options.webhookUrl !== undefined || options.channel !== undefined
}

export function hasEmailConfigChanges(options: {
  smtpHost?: string
  smtpPort?: string
  username?: string
  password?: string
  fromAddress?: string
  fromName?: string
  toAddresses?: string
}): boolean {
  return options.smtpHost !== undefined || options.smtpPort !== undefined
    || options.username !== undefined || options.password !== undefined
    || options.fromAddress !== undefined || options.fromName !== undefined
    || options.toAddresses !== undefined
}

export function hasWebhookConfigChanges(options: { url?: string; method?: string }): boolean {
  return options.url !== undefined || options.method !== undefined
}

/** Merges --webhook-url/--channel over the provider's existing config so unspecified fields survive a partial update. */
export function buildSlackConfigUpdate(
  options: { webhookUrl?: string; channel?: string },
  current: CurrentSlackConfig | null,
): SlackProviderConfig {
  return {
    webhook_url: options.webhookUrl ?? current?.webhook_url ?? '',
    channel: options.channel !== undefined ? (options.channel || null) : (current?.channel ?? null),
  }
}

/** Merges SMTP flags over the provider's existing config so unspecified fields survive a partial update. */
export function buildEmailConfigUpdate(
  options: {
    smtpHost?: string
    smtpPort?: string
    username?: string
    password?: string
    fromAddress?: string
    fromName?: string
    toAddresses?: string
  },
  current: CurrentEmailConfig | null,
): EmailProviderConfig {
  return {
    smtp_host: options.smtpHost ?? current?.smtp_host ?? '',
    smtp_port: options.smtpPort ? parseInt(options.smtpPort, 10) : (current?.smtp_port ?? 587),
    username: options.username ?? current?.username ?? '',
    password: options.password ?? current?.password ?? '',
    from_address: options.fromAddress ?? current?.from_address ?? '',
    from_name: options.fromName !== undefined ? (options.fromName || null) : (current?.from_name ?? null),
    to_addresses: options.toAddresses
      ? options.toAddresses.split(',').map((a) => a.trim())
      : (current?.to_addresses ?? []),
  }
}

/** Merges --url/--method over the provider's existing config so unspecified fields survive a partial update. */
export function buildWebhookConfigUpdate(
  options: { url?: string; method?: string },
  current: WebhookProviderConfig | null,
): WebhookProviderConfig {
  return {
    url: options.url ?? current?.url ?? '',
    method: options.method ?? current?.method ?? 'POST',
    headers: current?.headers ?? {},
    timeout_secs: current?.timeout_secs ?? 30,
  }
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

  const routes = notifications
    .command('routes')
    .description('Manage severity-based notification routes (which providers receive which severities)')

  routes
    .command('list')
    .alias('ls')
    .description('List notification routes')
    .option('--json', 'Output in JSON format')
    .action(listRoutes)

  routes
    .command('show')
    .description('Show notification route details')
    .requiredOption('--id <id>', 'Route ID')
    .option('--json', 'Output in JSON format')
    .action(showRoute)

  routes
    .command('create')
    .description('Create a notification route')
    .option('-n, --name <name>', 'Route name')
    .option('--min-severity <severity>', `Minimum severity: ${SEVERITIES.join(', ')}`)
    .option('--max-severity <severity>', `Maximum severity: ${SEVERITIES.join(', ')}`)
    .option('--provider-ids <ids>', 'Comma-separated notification provider IDs')
    .option('--enabled <enabled>', 'Enable or disable (true/false, default: true)')
    .option('--json', 'Output in JSON format')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createRoute)

  routes
    .command('update')
    .description('Update a notification route')
    .requiredOption('--id <id>', 'Route ID')
    .option('-n, --name <name>', 'New route name')
    .option('--min-severity <severity>', `Minimum severity: ${SEVERITIES.join(', ')}`)
    .option('--max-severity <severity>', `Maximum severity: ${SEVERITIES.join(', ')}`)
    .option('--provider-ids <ids>', 'Comma-separated notification provider IDs (replaces the current set)')
    .option('--enabled <enabled>', 'Enable or disable (true/false)')
    .option('--json', 'Output in JSON format')
    .action(updateRoute)

  routes
    .command('remove')
    .alias('rm')
    .description('Remove a notification route')
    .requiredOption('--id <id>', 'Route ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeRoute)
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
  const parsedEnabled = parseEnabledFlag(options.enabled)
  if (parsedEnabled === 'invalid') {
    warning('Invalid value for --enabled. Use "true" or "false"')
    return
  }
  const enabled = parsedEnabled

  // Check if any type-specific config options were provided
  const hasSlackConfig = hasSlackConfigChanges(options)
  const hasEmailConfig = hasEmailConfigChanges(options)
  const hasWebhookConfig = hasWebhookConfigChanges(options)

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
        config: buildSlackConfigUpdate(options, currentConfig),
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
        config: buildEmailConfigUpdate(options, currentConfig),
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
        config: buildWebhookConfigUpdate(options, currentConfig),
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

function routeColumns(): TableColumn<NotificationRoute>[] {
  return [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Range', accessor: (r) => `${r.min_severity} → ${r.max_severity}` },
    { header: 'Providers', accessor: (r) => r.provider_ids.join(', ') || '(none)' },
    { header: 'Status', accessor: (r) => r.enabled ? 'enabled' : 'disabled', color: (v) => statusBadge(v === 'enabled' ? 'active' : 'inactive') },
  ]
}

async function listRoutes(options: RouteListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const routesPage = await withSpinner('Fetching notification routes...', async () => {
    const { data, error } = await listNotificationRoutes({ client, query: { page: 1, page_size: 100 } })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })
  const items = routesPage?.items ?? []

  if (options.json) {
    json(items)
    return
  }

  newline()
  header(`${icons.info} Notification Routes (${items.length})`)

  if (items.length === 0) {
    info('No notification routes configured')
    info('Run: temps notifications routes create --name "Critical alerts" --min-severity critical --max-severity emergency --provider-ids <id> -y')
    newline()
    return
  }

  printTable(items, routeColumns(), { style: 'minimal' })
  newline()
}

async function showRoute(options: RouteShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid route ID')
    return
  }

  const route = await withSpinner('Fetching route...', async () => {
    const { data, error } = await getNotificationRoute({ client, path: { id } })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Route ${options.id} not found`)
    }
    return data
  })

  if (options.json) {
    json(route)
    return
  }

  newline()
  header(`${icons.info} ${route.name}`)
  keyValue('ID', route.id)
  keyValue('Severity range', `${route.min_severity} → ${route.max_severity}`)
  keyValue('Providers', route.provider_ids.join(', ') || '(none)')
  keyValue('Status', route.enabled ? colors.success('enabled') : colors.muted('disabled'))
  keyValue('Created', new Date(route.created_at * 1000).toLocaleString())
  keyValue('Updated', new Date(route.updated_at * 1000).toLocaleString())
  newline()
}

/** Resolves --provider-ids, or interactively prompts using the currently configured providers. */
async function resolveProviderIds(options: { providerIds?: string; yes?: boolean }): Promise<number[]> {
  if (options.providerIds) {
    return parseProviderIds(options.providerIds)
  }
  if (options.yes) {
    throw new Error('--provider-ids is required with -y/--yes')
  }

  const { data: providers, error } = await listNotificationProviders({ client })
  if (error) {
    throw new Error(getErrorMessage(error))
  }
  if (!providers || providers.length === 0) {
    throw new Error('No notification providers configured. Run: temps notifications add')
  }

  const selected = await promptCheckbox<number>({
    message: 'Providers to deliver to this route',
    choices: providers.map((p) => ({
      name: `${p.name} (${p.provider_type}, ${p.enabled ? 'enabled' : 'disabled'})`,
      value: p.id,
    })),
    required: true,
  })
  return selected
}

async function createRoute(options: RouteCreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const isAutomation = options.yes && options.name && options.minSeverity && options.maxSeverity && options.providerIds

  const name = options.name || await promptText({
    message: 'Route name',
    required: true,
  })

  const minSeverity = options.minSeverity || (isAutomation ? undefined : await promptSelect({
    message: 'Minimum severity',
    choices: SEVERITIES.map((s) => ({ name: s, value: s })),
    default: 'debug',
  }))
  if (!minSeverity) {
    warning('--min-severity is required with -y/--yes')
    return
  }

  const maxSeverity = options.maxSeverity || (isAutomation ? undefined : await promptSelect({
    message: 'Maximum severity',
    choices: SEVERITIES.map((s) => ({ name: s, value: s })),
    default: 'emergency',
  }))
  if (!maxSeverity) {
    warning('--max-severity is required with -y/--yes')
    return
  }

  const providerIds = await resolveProviderIds(options)

  const parsedEnabled = parseEnabledFlag(options.enabled)
  if (parsedEnabled === 'invalid') {
    warning('Invalid value for --enabled. Use "true" or "false"')
    return
  }

  const created = await withSpinner('Creating notification route...', async () => {
    const { data, error } = await createNotificationRoute({
      client,
      body: {
        name,
        min_severity: minSeverity,
        max_severity: maxSeverity,
        provider_ids: providerIds,
        enabled: parsedEnabled ?? true,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (options.json && created) {
    json(created)
    return
  }

  success(`Notification route "${created?.name ?? name}" created successfully`)
}

async function updateRoute(options: RouteUpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid route ID')
    return
  }

  const parsedEnabled = parseEnabledFlag(options.enabled)
  if (parsedEnabled === 'invalid') {
    warning('Invalid value for --enabled. Use "true" or "false"')
    return
  }

  const updated = await withSpinner('Updating notification route...', async () => {
    const { data, error } = await updateNotificationRoute({
      client,
      path: { id },
      body: {
        name: options.name ?? undefined,
        min_severity: options.minSeverity ?? undefined,
        max_severity: options.maxSeverity ?? undefined,
        provider_ids: options.providerIds ? parseProviderIds(options.providerIds) : undefined,
        enabled: parsedEnabled ?? undefined,
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

  success(`Notification route "${updated?.name ?? options.id}" updated successfully`)
}

async function removeRoute(options: RouteRemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.id, 10)
  if (isNaN(id)) {
    warning('Invalid route ID')
    return
  }

  const { data: route, error: getError } = await getNotificationRoute({ client, path: { id } })
  if (getError || !route) {
    warning(`Route ${options.id} not found`)
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove notification route "${route.name}"?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing route...', async () => {
    const { error } = await deleteNotificationRoute({ client, path: { id } })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Notification route removed')
}
