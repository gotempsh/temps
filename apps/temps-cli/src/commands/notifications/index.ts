import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listNotificationProviders,
  createSlackProvider,
  getNotificationProvider,
  deleteProvider2,
  testProvider2,
} from '../../api/sdk.gen.js'
import type { NotificationProviderResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptSelect, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface AddOptions {
  type?: string
  name?: string
  webhookUrl?: string
  channel?: string
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

export function registerNotificationsCommands(program: Command): void {
  const notifications = program
    .command('notifications')
    .alias('notify')
    .description('Manage notification providers (Slack, Email, etc.)')

  notifications
    .command('list')
    .alias('ls')
    .description('List configured notification providers')
    .option('--json', 'Output in JSON format')
    .action(listProviders)

  notifications
    .command('add')
    .description('Add a new notification provider')
    .option('-t, --type <type>', 'Provider type (slack, discord)')
    .option('-n, --name <name>', 'Provider name')
    .option('-w, --webhook-url <url>', 'Webhook URL')
    .option('-c, --channel <channel>', 'Channel name (optional)')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(addProvider)

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

  let providerType: string
  let name: string
  let webhookUrl: string
  let channel: string | null = null

  // Check if automation mode (all required params provided)
  const isAutomation = options.yes && options.type && options.name && options.webhookUrl

  if (isAutomation) {
    providerType = options.type!
    name = options.name!
    webhookUrl = options.webhookUrl!
    channel = options.channel || null

    if (providerType !== 'slack' && providerType !== 'discord') {
      warning(`Invalid provider type: ${providerType}. Supported: slack, discord`)
      return
    }
  } else {
    // Interactive mode
    providerType = options.type || await promptSelect({
      message: 'Notification provider type',
      choices: [
        { name: 'Slack', value: 'slack' },
        { name: 'Discord (via Slack webhook)', value: 'discord' },
      ],
    })

    if (providerType !== 'slack' && providerType !== 'discord') {
      warning(`Invalid provider type: ${providerType}. Supported: slack, discord`)
      return
    }

    name = options.name || await promptText({
      message: 'Provider name',
      default: `${providerType}-notifications`,
      required: true,
    })

    info('\nYou need a Slack Incoming Webhook URL.')
    if (providerType === 'slack') {
      info('Create one at: https://api.slack.com/messaging/webhooks')
    } else {
      info('For Discord: Server Settings > Integrations > Webhooks > Copy Webhook URL')
      info('Append /slack to the Discord webhook URL')
    }
    newline()

    webhookUrl = options.webhookUrl || await promptPassword({
      message: 'Webhook URL',
    })

    channel = options.channel || await promptText({
      message: 'Channel name (optional)',
      default: '',
    }) || null
  }

  await withSpinner(`Creating ${providerType} notification provider...`, async () => {
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

  success(`${providerType} notification provider created successfully`)
  info('Run: temps notifications test --id <id> to send a test notification')
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
