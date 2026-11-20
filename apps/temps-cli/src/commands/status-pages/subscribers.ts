// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Email subscriptions of a status page (ADR 108 D2).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  getStatusPageSubscriptions,
  updateStatusPageSubscriptions,
  listStatusPageSubscribers,
  deleteStatusPageSubscriber,
  exportStatusPageSubscribers,
} from '../../api/sdk.gen.js'
import type { SubscriptionSettingsResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, info, warning, keyValue, formatDate,
} from '../../ui/output.js'

interface SettingsOptions {
  id: string
  enable?: boolean
  disable?: boolean
  sender?: string
  json?: boolean
}

interface ListOptions {
  id: string
  page?: string
  pageSize?: string
  json?: boolean
}

interface DeleteOptions {
  id: string
  subscriberId: string
  yes?: boolean
}

interface ExportOptions {
  id: string
  output?: string
}

/** What still blocks subscriptions, one line each with where to fix it. */
export function subscriptionBlockerLines(settings: SubscriptionSettingsResponse): string[] {
  return [settings.email, settings.links]
    .filter((capability) => !capability.configured)
    .map((capability) => {
      const reason = capability.reason ?? 'Not configured'
      return capability.setup_path
        ? `${reason} (set it up in the console at ${capability.setup_path})`
        : reason
    })
}

/** The request body for `subscriptions set`, or an error message. */
export function settingsBody(
  options: Pick<SettingsOptions, 'enable' | 'disable' | 'sender'>,
): { enabled?: boolean; sender?: string } | string {
  if (options.enable && options.disable) {
    return 'Pass either --enable or --disable, not both'
  }
  const body: { enabled?: boolean; sender?: string } = {}
  if (options.enable) body.enabled = true
  if (options.disable) body.enabled = false
  if (options.sender !== undefined) body.sender = options.sender
  if (Object.keys(body).length === 0) {
    return 'Nothing to change: pass --enable, --disable or --sender <address>'
  }
  return body
}

function parseId(value: string, what: string): number | null {
  const id = parseInt(value, 10)
  if (isNaN(id)) {
    warning(`Invalid ${what}`)
    return null
  }
  return id
}

async function run<T>(label: string, call: () => Promise<{ data?: T; error?: unknown }>): Promise<T> {
  return withSpinner(label, async () => {
    const { data, error } = await call()
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    if (data === undefined) {
      throw new Error('The server returned an empty response')
    }
    return data
  })
}

function printSettings(settings: SubscriptionSettingsResponse): void {
  newline()
  header(`${icons.info} Email subscriptions of status page ${settings.status_page_id}`)
  keyValue('State', settings.enabled ? colors.success('on') : colors.muted('off'))
  keyValue('Sender', settings.sender ?? colors.muted('not set'))
  keyValue('Confirmed', settings.confirmed_count)
  keyValue('Waiting for confirmation', settings.pending_count)
  keyValue('Limit', settings.max_subscribers)
  if (settings.verified_domains.length > 0) {
    keyValue('Sender domains', settings.verified_domains.join(', '))
  }
  if (settings.enabled_at) {
    keyValue('On since', formatDate(settings.enabled_at))
  }
  for (const line of subscriptionBlockerLines(settings)) {
    warning(line)
  }
  if (!settings.enabled && subscriptionBlockerLines(settings).length === 0) {
    info(`Switch them on with: temps status-pages subscriptions set --id ${settings.status_page_id} --enable`
      + (settings.sender ? '' : ' --sender status@<verified-domain>'))
  }
  newline()
}

export function registerSubscriberCommands(pages: Command): void {
  const subscriptions = pages
    .command('subscriptions')
    .description('Let visitors subscribe to a status page by email')

  subscriptions
    .command('show')
    .description('Show whether visitors can subscribe, the sender and what is missing')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--json', 'Output in JSON format')
    .action(async (options: SettingsOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const settings = await run('Fetching subscription settings...', () =>
        getStatusPageSubscriptions({ client, path: { status_page_id: id } }))
      if (options.json) {
        json(settings)
        return
      }
      printSettings(settings)
    })

  subscriptions
    .command('set')
    .description('Switch subscriptions on or off, or set the sender address')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--enable', 'Show the subscribe form on the public page')
    .option('--disable', 'Hide the subscribe form and stop sending')
    .option('--sender <address>', 'From address at a verified email domain ("" clears it)')
    .option('--json', 'Output in JSON format')
    .action(async (options: SettingsOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const body = settingsBody(options)
      if (typeof body === 'string') {
        warning(body)
        return
      }
      const settings = await run('Saving subscription settings...', () =>
        updateStatusPageSubscriptions({ client, path: { status_page_id: id }, body }))
      if (options.json) {
        json(settings)
        return
      }
      success(settings.enabled ? 'Subscriptions are on' : 'Subscription settings saved')
      printSettings(settings)
    })

  const subscribers = pages
    .command('subscribers')
    .description('List, export or delete the subscribers of a status page')

  subscribers
    .command('list')
    .alias('ls')
    .description('List subscribers, newest first')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--page <n>', 'Page number', '1')
    .option('--page-size <n>', 'Items per page (max 100)', '20')
    .option('--json', 'Output in JSON format')
    .action(async (options: ListOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const result = await run('Fetching subscribers...', () =>
        listStatusPageSubscribers({
          client,
          path: { status_page_id: id },
          query: {
            page: parseInt(options.page ?? '1', 10) || 1,
            page_size: parseInt(options.pageSize ?? '20', 10) || 20,
          },
        }))
      if (options.json) {
        json(result)
        return
      }
      newline()
      if (result.items.length === 0) {
        info('No subscribers yet.')
        newline()
        return
      }
      printTable(result.items, [
        { header: 'ID', key: 'id', width: 8 },
        { header: 'Email', key: 'email', color: (v) => colors.bold(v) },
        { header: 'State', key: 'state' },
        { header: 'Subscribed', accessor: (s) => formatDate(s.created_at) },
      ], { style: 'minimal' })
      info(`Page ${result.page} of ${Math.max(1, Math.ceil(result.total / result.page_size))} (${result.total} total)`)
      newline()
    })

  subscribers
    .command('export')
    .description('Export every subscriber as CSV (email,state,subscribed_at,confirmed_at)')
    .requiredOption('--id <id>', 'Status page ID')
    .option('-o, --output <file>', 'Write to a file instead of standard output')
    .action(async (options: ExportOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const csv = await run('Exporting subscribers...', () =>
        exportStatusPageSubscribers({
          client,
          path: { status_page_id: id },
          parseAs: 'text',
        }) as Promise<{ data?: string; error?: unknown }>)
      if (options.output) {
        await Bun.write(options.output, csv)
        success(`Wrote ${options.output}`)
        return
      }
      process.stdout.write(csv)
    })

  subscribers
    .command('delete')
    .alias('rm')
    .description('Delete a subscriber; the address gets no more mail')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--subscriber-id <id>', 'Subscriber ID (see `subscribers list`)')
    .option('-y, --yes', 'Skip the confirmation prompt')
    .action(async (options: DeleteOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const subscriberId = parseId(options.subscriberId, 'subscriber ID')
      if (id === null || subscriberId === null) return
      if (!options.yes) {
        const confirmed = await promptConfirm({
          message: `Delete subscriber ${subscriberId}? The address is removed and gets no more mail.`,
          default: false,
        })
        if (!confirmed) {
          info('Cancelled')
          return
        }
      }
      await withSpinner('Deleting subscriber...', async () => {
        const { error } = await deleteStatusPageSubscriber({
          client,
          path: { status_page_id: id, subscriber_id: subscriberId },
        })
        if (error) {
          throw new Error(getErrorMessage(error))
        }
      })
      success(`Deleted subscriber ${subscriberId}`)
    })
}
