import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  listWebhooks,
  createWebhook,
  getWebhook,
  deleteWebhook,
  updateWebhook,
  listEventTypes,
} from '../../api/sdk.gen.js'
import type { WebhookResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptText, promptPassword, promptConfirm } from '../../ui/prompts.js'
import { newline, header, icons, json, colors, success, info, warning, keyValue } from '../../ui/output.js'

interface ListOptions {
  projectId: string
  json?: boolean
}

interface CreateOptions {
  projectId: string
  url?: string
  events?: string
  secret?: string
  yes?: boolean
}

interface ShowOptions {
  projectId: string
  webhookId: string
  json?: boolean
}

interface RemoveOptions {
  projectId: string
  webhookId: string
  force?: boolean
  yes?: boolean
}

interface EnableDisableOptions {
  projectId: string
  webhookId: string
}

export function registerWebhooksCommands(program: Command): void {
  const webhooks = program
    .command('webhooks')
    .alias('hooks')
    .description('Manage webhooks for project events')

  webhooks
    .command('list')
    .alias('ls')
    .description('List all webhooks for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listWebhooksAction)

  webhooks
    .command('create')
    .alias('add')
    .description('Create a new webhook for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('-u, --url <url>', 'Webhook URL')
    .option('-e, --events <events>', 'Comma-separated event types (or "all" for all events)')
    .option('-s, --secret <secret>', 'Webhook secret for signature verification')
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .action(createWebhookAction)

  webhooks
    .command('show')
    .description('Show webhook details')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .option('--json', 'Output in JSON format')
    .action(showWebhook)

  webhooks
    .command('remove')
    .alias('rm')
    .description('Delete a webhook')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeWebhook)

  webhooks
    .command('enable')
    .description('Enable a webhook')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .action((options: EnableDisableOptions) => toggleWebhook(options, true))

  webhooks
    .command('disable')
    .description('Disable a webhook')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .action((options: EnableDisableOptions) => toggleWebhook(options, false))

  webhooks
    .command('events')
    .description('List available webhook event types')
    .option('--json', 'Output in JSON format')
    .action(listEvents)
}

async function listWebhooksAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const id = parseInt(options.projectId, 10)
  if (isNaN(id)) {
    warning('Invalid project ID')
    return
  }

  const webhooksData = await withSpinner('Fetching webhooks...', async () => {
    const { data, error } = await listWebhooks({
      client,
      path: { project_id: id },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(webhooksData)
    return
  }

  newline()
  header(`${icons.info} Webhooks for Project ${id} (${webhooksData.length})`)

  if (webhooksData.length === 0) {
    info('No webhooks configured')
    info(`Run: temps webhooks create --project-id ${id} --url https://example.com/webhook --events all -y`)
    newline()
    return
  }

  const columns: TableColumn<WebhookResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'URL', key: 'url', color: (v) => colors.muted(v.length > 40 ? v.slice(0, 40) + '...' : v) },
    { header: 'Events', accessor: (w) => w.events.length.toString() },
    { header: 'Secret', accessor: (w) => w.has_secret ? 'Yes' : 'No' },
    { header: 'Status', accessor: (w) => w.enabled ? 'enabled' : 'disabled', color: (v) => statusBadge(v === 'enabled' ? 'active' : 'inactive') },
  ]

  printTable(webhooksData, columns, { style: 'minimal' })
  newline()
}

async function createWebhookAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let url: string
  let selectedEvents: string[]
  let secret: string | null = null

  // Get available event types
  const { data: eventTypesData } = await listEventTypes({ client })

  if (!eventTypesData || eventTypesData.length === 0) {
    warning('No event types available')
    return
  }

  // Extract event type strings
  const eventTypeStrings = eventTypesData.map(e => e.event_type)

  // Check if automation mode (all required params provided)
  const isAutomation = options.yes && options.url && options.events

  if (isAutomation) {
    url = options.url!
    secret = options.secret || null

    // Parse events
    if (options.events!.toLowerCase() === 'all') {
      selectedEvents = eventTypeStrings
    } else {
      selectedEvents = options.events!.split(',').map(e => e.trim())
      // Validate events
      for (const event of selectedEvents) {
        if (!eventTypeStrings.includes(event)) {
          warning(`Invalid event type: ${event}`)
          info(`Available events: ${eventTypeStrings.join(', ')}`)
          return
        }
      }
    }
  } else {
    // Interactive mode
    url = options.url || await promptText({
      message: 'Webhook URL',
      required: true,
    })

    info('\nAvailable event types:')
    eventTypesData.forEach((e, i) => console.log(`  ${i + 1}. ${e.event_type} - ${e.description}`))
    newline()

    const eventInput = options.events || await promptText({
      message: 'Select events (comma-separated numbers, or "all" for all events)',
      required: true,
    })

    if (eventInput.toLowerCase() === 'all') {
      selectedEvents = eventTypeStrings
    } else {
      const indices = eventInput.split(',').map(s => parseInt(s.trim(), 10) - 1)
      selectedEvents = indices
        .filter(i => i >= 0 && i < eventTypesData.length)
        .map(i => eventTypesData[i]!.event_type)
    }

    if (selectedEvents.length === 0) {
      warning('No events selected')
      return
    }

    secret = options.secret || await promptPassword({
      message: 'Webhook secret (optional, for signature verification)',
    }) || null
  }

  await withSpinner('Creating webhook...', async () => {
    const { error } = await createWebhook({
      client,
      path: { project_id: projectId },
      body: {
        url,
        events: selectedEvents,
        secret,
        enabled: true,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Webhook created successfully')
  info(`Subscribed to ${selectedEvents.length} event(s)`)
}

async function showWebhook(options: ShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const hookId = parseInt(options.webhookId, 10)
  if (isNaN(projId) || isNaN(hookId)) {
    warning('Invalid project or webhook ID')
    return
  }

  const webhook = await withSpinner('Fetching webhook...', async () => {
    const { data, error } = await getWebhook({
      client,
      path: { project_id: projId, webhook_id: hookId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Webhook ${options.webhookId} not found`)
    }
    return data
  })

  if (options.json) {
    json(webhook)
    return
  }

  newline()
  header(`${icons.info} Webhook #${webhook.id}`)
  keyValue('URL', webhook.url)
  keyValue('Status', webhook.enabled ? colors.success('Enabled') : colors.muted('Disabled'))
  keyValue('Has Secret', webhook.has_secret ? 'Yes' : 'No')
  keyValue('Project ID', webhook.project_id)
  keyValue('Created', new Date(webhook.created_at).toLocaleString())
  keyValue('Updated', new Date(webhook.updated_at).toLocaleString())

  newline()
  header('Subscribed Events')
  for (const event of webhook.events) {
    console.log(`  • ${event}`)
  }
  newline()
}

async function removeWebhook(options: RemoveOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const hookId = parseInt(options.webhookId, 10)
  if (isNaN(projId) || isNaN(hookId)) {
    warning('Invalid project or webhook ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Delete webhook #${options.webhookId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Deleting webhook...', async () => {
    const { error } = await deleteWebhook({
      client,
      path: { project_id: projId, webhook_id: hookId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Webhook deleted')
}

async function toggleWebhook(options: EnableDisableOptions, enabled: boolean): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const hookId = parseInt(options.webhookId, 10)
  if (isNaN(projId) || isNaN(hookId)) {
    warning('Invalid project or webhook ID')
    return
  }

  await withSpinner(`${enabled ? 'Enabling' : 'Disabling'} webhook...`, async () => {
    const { error } = await updateWebhook({
      client,
      path: { project_id: projId, webhook_id: hookId },
      body: {
        enabled,
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success(`Webhook ${enabled ? 'enabled' : 'disabled'}`)
}

async function listEvents(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const events = await withSpinner('Fetching event types...', async () => {
    const { data, error } = await listEventTypes({ client })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(events)
    return
  }

  newline()
  header(`${icons.info} Available Event Types (${events.length})`)

  if (events.length === 0) {
    info('No event types available')
    newline()
    return
  }

  // Group events by category
  const categories = new Map<string, typeof events>()
  for (const event of events) {
    const cat = event.category || 'Other'
    if (!categories.has(cat)) {
      categories.set(cat, [])
    }
    categories.get(cat)!.push(event)
  }

  for (const [category, categoryEvents] of categories) {
    console.log(`\n  ${colors.bold(category)}:`)
    for (const event of categoryEvents) {
      console.log(`    ${colors.muted(event.event_type)} - ${event.description}`)
    }
  }
  newline()
}
