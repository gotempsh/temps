// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Signed webhooks and the page password of a status page (ADR 108 D2).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createStatusPageWebhook,
  deleteStatusPageWebhook,
  getStatusPagePassword,
  getStatusPageWebhook,
  listStatusPageWebhooks,
  setStatusPagePassword,
  updateStatusPageWebhook,
} from '../../api/sdk.gen.js'
import { endpoint } from '../../lib/endpoint.js'
import type { UpdateStatusPageWebhookRequest, StatusPageWebhookResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { promptConfirm, promptPassword } from '../../ui/prompts.js'
import { newline, json, colors, success, info, warning, keyValue, formatDate } from '../../ui/output.js'

export const MIN_PASSWORD_CHARS = 8
export const MAX_PASSWORD_CHARS = 256

interface ListOptions {
  id: string
  json?: boolean
}

interface AddOptions {
  id: string
  url: string
  json?: boolean
}

interface UpdateOptions {
  id: string
  webhookId: string
  url?: string
  enable?: boolean
  disable?: boolean
  json?: boolean
}

interface RemoveOptions {
  id: string
  webhookId: string
  yes?: boolean
}

interface PasswordOptions {
  id: string
  passwordStdin?: boolean
  remove?: boolean
  json?: boolean
}

/** Why `url` cannot be a webhook URL, or null. The server also checks the address. */
export function webhookUrlProblem(url: string): string | null {
  const trimmed = url.trim()
  if (!trimmed) return 'The URL is empty'
  if (trimmed.length > 2048) return 'The URL is longer than 2048 characters'
  let parsed: URL
  try {
    parsed = new URL(trimmed)
  } catch {
    return `"${trimmed}" is not a valid URL`
  }
  if (parsed.protocol !== 'https:') return 'Webhook URLs must use https://'
  if (parsed.username || parsed.password) return 'The URL cannot carry a user name or password; verify the signature instead'
  if (parsed.hash) return 'The URL cannot have a #fragment'
  return null
}

/** The request body for `webhooks update`, or an error message. */
export function webhookUpdateBody(
  options: Pick<UpdateOptions, 'url' | 'enable' | 'disable'>,
): UpdateStatusPageWebhookRequest | string {
  if (options.enable && options.disable) return 'Pass either --enable or --disable, not both'
  const body: UpdateStatusPageWebhookRequest = {}
  if (options.url !== undefined) {
    const problem = webhookUrlProblem(options.url)
    if (problem) return problem
    body.url = options.url.trim()
  }
  if (options.enable) body.enabled = true
  if (options.disable) body.enabled = false
  if (Object.keys(body).length === 0) return 'Nothing to change: pass --url, --enable or --disable'
  return body
}

/** Why `password` cannot protect a page, or null. */
export function passwordProblem(password: string): string | null {
  const chars = [...password].length
  if (chars < MIN_PASSWORD_CHARS) return `A page password has at least ${MIN_PASSWORD_CHARS} characters`
  if (chars > MAX_PASSWORD_CHARS) return `A page password has at most ${MAX_PASSWORD_CHARS} characters`
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(password)) return 'A page password cannot contain control characters'
  return null
}

/** One line on how deliveries to a webhook are going. */
export function deliveryLine(webhook: StatusPageWebhookResponse): string {
  if (!webhook.enabled) return 'paused'
  if (webhook.last_failure_reason) return `failing: ${webhook.last_failure_reason}`
  if (webhook.last_success_at) return `delivered ${formatDate(webhook.last_success_at)}`
  return 'no event yet'
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

// A password of MAX_PASSWORD_CHARS characters is at most four UTF-8 bytes per
// character, plus a line ending.
export const MAX_PASSWORD_STDIN_BYTES = MAX_PASSWORD_CHARS * 4 + 2

// The first line of `input`, read only up to its first newline: a large or
// endless stream is never buffered. Returns null when no newline (or end of
// input) arrives within `limit` bytes.
export async function readFirstLine(
  input: AsyncIterable<string | Uint8Array>,
  limit = MAX_PASSWORD_STDIN_BYTES,
): Promise<string | null> {
  const chunks: Buffer[] = []
  let size = 0
  for await (const chunk of input) {
    const bytes = Buffer.from(chunk)
    const newline = bytes.indexOf(0x0a)
    const part = newline === -1 ? bytes : bytes.subarray(0, newline)
    if (size + part.length > limit) return null
    chunks.push(part)
    size += part.length
    if (newline !== -1) break
  }
  return Buffer.concat(chunks).toString('utf8').replace(/\r$/, '')
}

export function registerWebhookCommands(pages: Command): void {
  const webhooks = pages
    .command('webhooks')
    .description('Post every public update of a status page to signed HTTPS webhooks')

  endpoint(webhooks, getStatusPageWebhook, {
    name: 'get',
    description: 'Show one webhook of a status page',
    path: { status_page_id: 'int', webhook_id: 'int' },
  })

  webhooks
    .command('list')
    .alias('ls')
    .description('List the webhooks of a status page and how deliveries are going')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--json', 'Output in JSON format')
    .action(async (options: ListOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const result = await run('Fetching webhooks...', () =>
        listStatusPageWebhooks({ client, path: { status_page_id: id }, query: { page: 1, page_size: 100 } }))
      if (options.json) {
        json(result)
        return
      }
      newline()
      if (result.items.length === 0) {
        info(`No webhooks. Add one with: temps status-pages webhooks add --id ${id} --url https://hooks.example.com/status`)
        newline()
        return
      }
      printTable(result.items, [
        { header: 'ID', key: 'id', width: 8 },
        { header: 'URL', key: 'url', color: (v) => colors.bold(v) },
        { header: 'Deliveries', accessor: deliveryLine },
        { header: 'Added', accessor: (w) => formatDate(w.created_at) },
      ], { style: 'minimal' })
      newline()
    })

  webhooks
    .command('add')
    .description('Add a webhook; its signing secret is printed once')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--url <url>', 'https:// URL with a public address')
    .option('--json', 'Output in JSON format')
    .action(async (options: AddOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const problem = webhookUrlProblem(options.url)
      if (problem) {
        warning(problem)
        return
      }
      const created = await run('Adding webhook...', () =>
        createStatusPageWebhook({ client, path: { status_page_id: id }, body: { url: options.url.trim() } }))
      if (options.json) {
        json(created)
        return
      }
      success(`Added webhook ${created.webhook.id}`)
      keyValue('Signing secret', created.secret)
      warning('Store the secret now: it is not shown again. Verify X-Temps-Signature with it.')
    })

  webhooks
    .command('update')
    .description('Change the URL, or pause and resume deliveries')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID (see `webhooks list`)')
    .option('--url <url>', 'New https:// URL')
    .option('--enable', 'Resume deliveries')
    .option('--disable', 'Pause deliveries; those still waiting are dropped')
    .option('--json', 'Output in JSON format')
    .action(async (options: UpdateOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const webhookId = parseId(options.webhookId, 'webhook ID')
      if (id === null || webhookId === null) return
      const body = webhookUpdateBody(options)
      if (typeof body === 'string') {
        warning(body)
        return
      }
      const updated = await run('Saving webhook...', () =>
        updateStatusPageWebhook({ client, path: { status_page_id: id, webhook_id: webhookId }, body }))
      if (options.json) {
        json(updated)
        return
      }
      success(`Webhook ${updated.id} ${updated.enabled ? 'is on' : 'is paused'}`)
    })

  webhooks
    .command('remove')
    .alias('rm')
    .description('Delete a webhook; events waiting for it are dropped')
    .requiredOption('--id <id>', 'Status page ID')
    .requiredOption('--webhook-id <id>', 'Webhook ID (see `webhooks list`)')
    .option('-y, --yes', 'Skip the confirmation prompt')
    .action(async (options: RemoveOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      const webhookId = parseId(options.webhookId, 'webhook ID')
      if (id === null || webhookId === null) return
      if (!options.yes) {
        const confirmed = await promptConfirm({ message: `Delete webhook ${webhookId}?`, default: false })
        if (!confirmed) {
          info('Kept')
          return
        }
      }
      await withSpinner('Deleting webhook...', async () => {
        const { error } = await deleteStatusPageWebhook({
          client,
          path: { status_page_id: id, webhook_id: webhookId },
        })
        if (error) {
          throw new Error(getErrorMessage(error))
        }
      })
      success(`Deleted webhook ${webhookId}`)
    })

  const password = pages
    .command('password')
    .description('Protect a status page with a password')

  password
    .command('show')
    .description('Show whether a status page is password-protected')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--json', 'Output in JSON format')
    .action(async (options: ListOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      const result = await run('Fetching password state...', () =>
        getStatusPagePassword({ client, path: { status_page_id: id } }))
      if (options.json) {
        json(result)
        return
      }
      info(result.has_password
        ? `Status page ${id} asks visitors for a password; its feed is off.`
        : `Status page ${id} is open to anyone with its address.`)
    })

  password
    .command('set')
    .description('Set or change the password (prompted, or read from stdin); signs every visitor out')
    .requiredOption('--id <id>', 'Status page ID')
    .option('--password-stdin', 'Read the password from the first line of stdin')
    .option('--remove', 'Remove the password and open the page again')
    .option('--json', 'Output in JSON format')
    .action(async (options: PasswordOptions) => {
      await requireAuth()
      await setupClient()
      const id = parseId(options.id, 'status page ID')
      if (id === null) return
      if (options.remove && options.passwordStdin) {
        warning('Pass either --remove or --password-stdin, not both')
        return
      }
      let value: string | null = null
      if (!options.remove) {
        if (options.passwordStdin) {
          if (process.stdin.isTTY) {
            warning('--password-stdin reads from stdin, but stdin is a terminal: pipe the password in')
            return
          }
          value = await readFirstLine(process.stdin)
          if (value === null) {
            warning(`The password on stdin is longer than ${MAX_PASSWORD_CHARS} characters`)
            return
          }
        } else {
          if (!process.stdin.isTTY) {
            warning('Not a terminal: pass --password-stdin and pipe the password in')
            return
          }
          value = await promptPassword({
            message: 'Page password',
            validate: (input) => passwordProblem(input) ?? true,
            toStderr: options.json,
          })
        }
        const problem = passwordProblem(value)
        if (problem) {
          warning(problem)
          return
        }
      }
      const result = await run('Saving password...', () =>
        setStatusPagePassword({ client, path: { status_page_id: id }, body: { password: value } }))
      if (options.json) {
        json(result)
        return
      }
      success(result.has_password
        ? 'The page now asks for a password; every visitor was signed out'
        : 'The page is open again')
    })
}
