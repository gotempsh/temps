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
  listDeliveries,
  getDelivery,
  retryDelivery,
} from '../../api/sdk.gen.js'
import type { WebhookResponse, WebhookDeliveryResponse } from '../../api/types.gen.js'
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

interface UpdateOptions {
  projectId: string
  webhookId: string
  url?: string
  events?: string
  secret?: string
}

interface DeliveriesListOptions {
  projectId: string
  webhookId: string
  limit?: string
  json?: boolean
}

interface DeliveriesShowOptions {
  projectId: string
  webhookId: string
  deliveryId: string
  json?: boolean
}

interface DeliveriesRetryOptions {
  projectId: string
  webhookId: string
  deliveryId: string
}

/** Resolves --events for non-interactive `create`: "all" or a validated comma list. */
export function resolveCreateEvents(
  input: string,
  allEventTypes: string[],
): { events: string[] } | { invalidEvent: string } {
  if (input.toLowerCase() === 'all') {
    return { events: allEventTypes }
  }
  const events = input.split(',').map((e) => e.trim())
  const invalidEvent = events.find((event) => !allEventTypes.includes(event))
  if (invalidEvent) {
    return { invalidEvent }
  }
  return { events }
}

/** Resolves the interactive `create` prompt's 1-based index list, dropping out-of-range picks. */
export function selectEventsByIndex(
  input: string,
  eventTypes: Array<{ event_type: string }>,
): string[] {
  const indices = input.split(',').map((s) => parseInt(s.trim(), 10) - 1)
  return indices
    .filter((i) => i >= 0 && i < eventTypes.length)
    .map((i) => eventTypes[i]!.event_type)
}

/** Resolves --events for `update`: "all" or a comma list — unlike create, not validated against known types. */
export function parseWebhookEventsList(input: string, allEventTypes: string[]): string[] {
  return input.toLowerCase() === 'all'
    ? allEventTypes
    : input.split(',').map((e) => e.trim())
}

export function groupEventsByCategory<T extends { category?: string | null }>(
  events: T[],
): Map<string, T[]> {
  const categories = new Map<string, T[]>()
  for (const event of events) {
    const cat = event.category || 'Other'
    if (!categories.has(cat)) {
      categories.set(cat, [])
    }
    categories.get(cat)!.push(event)
  }
  return categories
}

export function formatDeliveryPayload(payload: string): string {
  try {
    return JSON.stringify(JSON.parse(payload), null, 2)
  } catch {
    return payload
  }
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
    .command('update')
    .description('Update a webhook')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .option('-u, --url <url>', 'New webhook URL')
    .option('-e, --events <events>', 'Comma-separated event types (or "all" for all events)')
    .option('-s, --secret <secret>', 'New webhook secret for signature verification')
    .action(updateWebhookAction)

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

  // Deliveries subcommand group
  const deliveries = webhooks
    .command('deliveries')
    .description('Manage webhook deliveries')

  deliveries
    .command('list')
    .alias('ls')
    .description('List deliveries for a webhook')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .option('--limit <n>', 'Number of deliveries to return (default: 50)')
    .option('--json', 'Output in JSON format')
    .action(listDeliveriesAction)

  deliveries
    .command('show')
    .description('Show delivery details')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .requiredOption('--delivery-id <id>', 'Delivery ID')
    .option('--json', 'Output in JSON format')
    .action(showDeliveryAction)

  deliveries
    .command('retry')
    .description('Retry a failed delivery')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID')
    .requiredOption('--delivery-id <id>', 'Delivery ID')
    .action(retryDeliveryAction)
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
    const resolved = resolveCreateEvents(options.events!, eventTypeStrings)
    if ('invalidEvent' in resolved) {
      warning(`Invalid event type: ${resolved.invalidEvent}`)
      info(`Available events: ${eventTypeStrings.join(', ')}`)
      return
    }
    selectedEvents = resolved.events
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
      selectedEvents = selectEventsByIndex(eventInput, eventTypesData)
    }

    if (selectedEvents.length === 0) {
      warning('No events selected')
      return
    }

    secret = options.secret || await promptPassword({
      message: 'Webhook secret (optional, for signature verification)',
    }) || null
  }

  const webhook = await withSpinner('Creating webhook...', async () => {
    const { data, error } = await createWebhook({
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
    return data
  })

  success('Webhook created successfully')
  if (webhook) {
    json(webhook)
  }
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

async function updateWebhookAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const hookId = parseInt(options.webhookId, 10)
  if (isNaN(projId) || isNaN(hookId)) {
    warning('Invalid project or webhook ID')
    return
  }

  const body: Record<string, unknown> = {}

  if (options.url) {
    body.url = options.url
  }

  if (options.events) {
    let allEventTypes: string[] = []
    if (options.events.toLowerCase() === 'all') {
      // Fetch available event types to resolve "all"
      const { data: eventTypesData } = await listEventTypes({ client })
      if (!eventTypesData || eventTypesData.length === 0) {
        warning('No event types available')
        return
      }
      allEventTypes = eventTypesData.map(e => e.event_type)
    }
    body.events = parseWebhookEventsList(options.events, allEventTypes)
  }

  if (options.secret) {
    body.secret = options.secret
  }

  if (Object.keys(body).length === 0) {
    warning('No fields to update. Provide at least one of --url, --events, or --secret')
    return
  }

  const webhook = await withSpinner('Updating webhook...', async () => {
    const { data, error } = await updateWebhook({
      client,
      path: { project_id: projId, webhook_id: hookId },
      body,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  success(`Webhook #${hookId} updated`)

  if (options.url) {
    info(`URL: ${options.url}`)
  }
  if (body.events) {
    info(`Events: ${(body.events as string[]).length} event(s)`)
  }
  if (options.secret) {
    info('Secret: updated')
  }
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
  const categories = groupEventsByCategory(events)

  for (const [category, categoryEvents] of categories) {
    console.log(`\n  ${colors.bold(category)}:`)
    for (const event of categoryEvents) {
      console.log(`    ${colors.muted(event.event_type)} - ${event.description}`)
    }
  }
  newline()
}

async function listDeliveriesAction(options: DeliveriesListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const hookId = parseInt(options.webhookId, 10)
  if (isNaN(projId) || isNaN(hookId)) {
    warning('Invalid project or webhook ID')
    return
  }

  const limit = options.limit ? parseInt(options.limit, 10) : undefined

  const deliveriesData = await withSpinner('Fetching deliveries...', async () => {
    const { data, error } = await listDeliveries({
      client,
      path: { project_id: projId, webhook_id: hookId },
      query: {
        ...(limit && { limit }),
      },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(deliveriesData)
    return
  }

  newline()
  header(`${icons.info} Deliveries for Webhook #${hookId} (${deliveriesData.length})`)

  if (deliveriesData.length === 0) {
    info('No deliveries found')
    newline()
    return
  }

  const columns: TableColumn<WebhookDeliveryResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Event', key: 'event_type' },
    { header: 'Status', accessor: (d) => d.success ? 'success' : 'failed', color: (v) => statusBadge(v === 'success' ? 'active' : 'error') },
    { header: 'Code', accessor: (d) => d.status_code != null ? d.status_code.toString() : '-' },
    { header: 'Attempt', accessor: (d) => d.attempt_number.toString() },
    { header: 'Delivered', accessor: (d) => d.delivered_at ? new Date(d.delivered_at).toLocaleString() : '-' },
  ]

  printTable(deliveriesData, columns, { style: 'minimal' })
  newline()
}

async function showDeliveryAction(options: DeliveriesShowOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const hookId = parseInt(options.webhookId, 10)
  const delId = parseInt(options.deliveryId, 10)
  if (isNaN(projId) || isNaN(hookId) || isNaN(delId)) {
    warning('Invalid project, webhook, or delivery ID')
    return
  }

  const delivery = await withSpinner('Fetching delivery...', async () => {
    const { data, error } = await getDelivery({
      client,
      path: { project_id: projId, webhook_id: hookId, delivery_id: delId },
    })
    if (error || !data) {
      throw new Error(getErrorMessage(error) ?? `Delivery ${options.deliveryId} not found`)
    }
    return data
  })

  if (options.json) {
    json(delivery)
    return
  }

  newline()
  header(`${icons.info} Delivery #${delivery.id}`)
  keyValue('Webhook ID', delivery.webhook_id)
  keyValue('Event Type', delivery.event_type)
  keyValue('Event ID', delivery.event_id)
  keyValue('Status', delivery.success ? colors.success('Success') : colors.error('Failed'))
  keyValue('Status Code', delivery.status_code != null ? delivery.status_code.toString() : '-')
  keyValue('Attempt', delivery.attempt_number.toString())
  keyValue('Created', new Date(delivery.created_at).toLocaleString())
  keyValue('Delivered', delivery.delivered_at ? new Date(delivery.delivered_at).toLocaleString() : '-')

  if (delivery.error_message) {
    newline()
    header('Error')
    console.log(`  ${colors.error(delivery.error_message)}`)
  }

  newline()
  header('Payload')
  console.log(formatDeliveryPayload(delivery.payload))
  newline()
}

async function retryDeliveryAction(options: DeliveriesRetryOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projId = parseInt(options.projectId, 10)
  const hookId = parseInt(options.webhookId, 10)
  const delId = parseInt(options.deliveryId, 10)
  if (isNaN(projId) || isNaN(hookId) || isNaN(delId)) {
    warning('Invalid project, webhook, or delivery ID')
    return
  }

  const delivery = await withSpinner('Retrying delivery...', async () => {
    const { data, error } = await retryDelivery({
      client,
      path: { project_id: projId, webhook_id: hookId, delivery_id: delId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (delivery?.success) {
    success(`Delivery #${delId} retried successfully`)
  } else {
    warning(`Delivery #${delId} retry attempted but delivery failed`)
    if (delivery?.error_message) {
      info(`Error: ${delivery.error_message}`)
    }
  }
}
