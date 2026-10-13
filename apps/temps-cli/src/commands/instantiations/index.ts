// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Template creations and imports run on the server, step by step. These
 * commands follow one until it finishes, show where it stopped, and retry it
 * from the step that failed.
 */

import type { Command } from 'commander'
import { getInstantiation, listInstantiations, retryInstantiation } from '../../api/sdk.gen.js'
import type { InstantiationResponse } from '../../api/types.gen.js'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { colors, formatRelativeTime, header, icons, info, json, keyValue, newline, warning } from '../../ui/output.js'
import { isQuietMode, setQuietMode, startSpinner, stopSpinner } from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { resolveGroup } from '../project-groups/index.js'

const CLI = 'bunx @temps-sdk/cli'
const POLL_INTERVAL_MS = 1500
/** How long the CLI waits by default before handing the user a command to keep following. */
export const DEFAULT_WAIT_MINUTES = 30

function unwrap<T>(result: { data?: T; error?: unknown }): T {
  if (result.error !== undefined || result.data === undefined) {
    throw new Error(getErrorMessage(result.error))
  }
  return result.data
}

export function isFinished(instantiation: Pick<InstantiationResponse, 'status'>): boolean {
  return instantiation.status === 'succeeded' || instantiation.status === 'failed' || instantiation.status === 'interrupted'
}

/** One line per step: ✓ done, ✗ failed (with why), · still to do. */
export function describeSteps(instantiation: Pick<InstantiationResponse, 'steps'>): string[] {
  return instantiation.steps.map((step) => {
    if (step.status === 'done') return `  ${colors.success('✓')} ${step.label}`
    if (step.status === 'failed') return `  ${colors.error('✗')} ${step.label}: ${step.error ?? 'failed'}`
    return `  ${colors.muted('·')} ${colors.muted(step.label)}`
  })
}

/** What to run next for an instantiation that did not succeed. */
export function nextCommands(instantiation: Pick<InstantiationResponse, 'id' | 'status'>): string[] {
  if (instantiation.status === 'failed' || instantiation.status === 'interrupted') {
    return [`${CLI} instantiations retry ${instantiation.id}`]
  }
  if (instantiation.status === 'pending' || instantiation.status === 'running') {
    return [`${CLI} instantiations show ${instantiation.id} --wait`]
  }
  return []
}

/** A positive whole number from a flag, or a clear error naming the flag. */
export function parsePositiveInt(value: string, flag: string): number {
  const trimmed = value.trim()
  const parsed = Number(trimmed)
  if (!/^\d+$/.test(trimmed) || !Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${flag} must be a positive whole number, got "${value}"`)
  }
  return parsed
}

/** `--timeout <minutes>` in milliseconds; the default when omitted. */
export function waitTimeoutMs(minutes?: string): number {
  return (minutes === undefined ? DEFAULT_WAIT_MINUTES : parsePositiveInt(minutes, '--timeout')) * 60_000
}

export interface WaitOptions {
  /** Stop waiting after this long; the instantiation keeps running on the server. */
  timeoutMs?: number
  /** Say which instantiation is being followed before polling starts. */
  announce?: boolean
  intervalMs?: number
  /** Loads the current state; the API by default. */
  load?: (id: number) => Promise<InstantiationResponse>
}

function followUpCommand(id: number): string {
  return `${CLI} instantiations show ${id} --wait`
}

/** A line for the user; on stderr under --json so stdout stays one JSON document. */
function notice(message: string): void {
  if (isQuietMode()) process.stderr.write(`${message}\n`)
  else info(message)
}

/**
 * Poll until the instantiation finishes, showing the step in progress. When
 * the timeout passes first, say how to keep following it and return its
 * current (still running) state: `report` then exits non-zero.
 */
export async function waitForInstantiation(id: number, options: WaitOptions = {}): Promise<InstantiationResponse> {
  const timeoutMs = options.timeoutMs ?? DEFAULT_WAIT_MINUTES * 60_000
  const intervalMs = options.intervalMs ?? POLL_INTERVAL_MS
  const load = options.load ?? (async (value: number) => unwrap(await getInstantiation({ client, path: { id: value } })))
  if (options.announce) {
    notice(`Started (id ${id}). If this stops before it finishes, follow it with: ${followUpCommand(id)}`)
  }
  const deadline = Date.now() + timeoutMs
  const spinner = startSpinner('Starting...')
  let current: InstantiationResponse
  try {
    for (;;) {
      current = await load(id)
      if (isFinished(current)) return current
      if (Date.now() >= deadline) break
      const next = current.steps.find((step) => step.status !== 'done')
      const done = current.steps.filter((step) => step.status === 'done').length
      spinner.text = `${next?.label ?? 'Finishing'}... (${done}/${current.steps.length})`
      await new Promise((resolve) => setTimeout(resolve, Math.min(intervalMs, Math.max(0, deadline - Date.now()))))
    }
  } finally {
    stopSpinner()
  }
  const minutes = Math.max(1, Math.round(timeoutMs / 60_000))
  warning(
    `Stopped waiting after ${minutes} minute(s); it is still ${current.status} on the server (id ${id}). Follow it with: ${followUpCommand(id)}`
  )
  return current
}

/** Print an instantiation and set the exit code when it did not succeed. */
export function report(instantiation: InstantiationResponse, options: { json?: boolean }): void {
  if (options.json) {
    json(instantiation)
  } else {
    newline()
    const title =
      instantiation.status === 'succeeded'
        ? `${icons.success} ${instantiation.name}`
        : `${instantiation.name} (${instantiation.status})`
    header(title)
    for (const line of describeSteps(instantiation)) console.log(line)
    const created = instantiation.services.filter((service) => service.project_id != null)
    if (created.length > 0) {
      newline()
      keyValue('Services', created.map((service) => service.slug).join(', '))
    }
    if (instantiation.error && instantiation.status !== 'succeeded') {
      newline()
      info(`Stopped: ${instantiation.error}`)
    }
    for (const command of nextCommands(instantiation)) info(`Continue with: ${command}`)
    newline()
  }
  if (instantiation.status !== 'succeeded') process.exitCode = 1
}

export function registerInstantiationsCommands(program: Command): void {
  const cmd = program
    .command('instantiations')
    .alias('inst')
    .description('Follow and retry template creations and imports')

  cmd
    .command('list')
    .alias('ls')
    .description('List your recent template creations and imports, newest first')
    .option('--group <project>', 'Only those that added services to this project (group slug or id)')
    .option('--page <n>', 'Page number (default 1)')
    .option('--per-page <n>', 'How many per page (default 20, at most 100)')
    .option('--limit <n>', 'Same as --per-page')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  cmd
    .command('show <id>')
    .alias('get')
    .description('Show every step of a template creation or import')
    .option('--wait', 'Wait until it finishes')
    .option('--timeout <minutes>', `With --wait: stop waiting after this long (default ${DEFAULT_WAIT_MINUTES})`)
    .option('--json', 'Output in JSON format')
    .action(showAction)

  cmd
    .command('retry <id>')
    .description('Continue from the step that failed; finished steps are not repeated')
    .option('--no-wait', 'Return right away instead of waiting until it finishes')
    .option('--timeout <minutes>', `Stop waiting after this long (default ${DEFAULT_WAIT_MINUTES}); it keeps running on the server`)
    .option('--json', 'Output in JSON format')
    .action(retryAction)
}

function parseId(value: string): number {
  const trimmed = value.trim()
  if (!/^\d+$/.test(trimmed) || Number(trimmed) <= 0) {
    throw new Error(`"${value}" is not an instantiation id (a number from: ${CLI} instantiations list)`)
  }
  return Number(trimmed)
}

export interface InstantiationPage {
  items: InstantiationResponse[]
  /** Unknown when the server predates paging (it returned a bare list). */
  total?: number
}

/**
 * The list response: a page object (`{ data | items | instantiations, total }`)
 * from current servers, or a bare array from servers that only took `limit`.
 */
export function toInstantiationPage(raw: unknown): InstantiationPage {
  if (Array.isArray(raw)) return { items: raw as InstantiationResponse[] }
  if (raw && typeof raw === 'object') {
    const page = raw as Record<string, unknown>
    const items = [page.data, page.items, page.instantiations].find(Array.isArray) as InstantiationResponse[] | undefined
    if (items) {
      return { items, total: typeof page.total === 'number' ? page.total : undefined }
    }
  }
  throw new Error('The server returned an unexpected instantiation list; upgrade the CLI or the server so they match')
}

/** `--per-page` and its alias `--limit` must agree when both are given. */
export function resolveListPaging(options: { page?: string; perPage?: string; limit?: string }): { page: number; perPage: number } {
  const page = options.page === undefined ? 1 : parsePositiveInt(options.page, '--page')
  const perPage = options.perPage === undefined ? undefined : parsePositiveInt(options.perPage, '--per-page')
  const limit = options.limit === undefined ? undefined : parsePositiveInt(options.limit, '--limit')
  if (perPage !== undefined && limit !== undefined && perPage !== limit) {
    throw new Error('--limit is the same as --per-page; pass one of them')
  }
  const size = perPage ?? limit ?? 20
  if (size > 100) throw new Error(`--per-page can be at most 100, got ${size}`)
  return { page, perPage: size }
}

async function resolveGroupId(ref: string): Promise<number> {
  const trimmed = ref.trim()
  if (/^\d+$/.test(trimmed) && Number(trimmed) > 0) return Number(trimmed)
  return (await resolveGroup(trimmed)).id
}

async function listAction(options: {
  group?: string
  page?: string
  perPage?: string
  limit?: string
  json?: boolean
}): Promise<void> {
  // Bad flags fail before anything is sent.
  const { page, perPage } = resolveListPaging(options)
  await requireAuth()
  await setupClient()
  const groupId = options.group === undefined ? undefined : await resolveGroupId(options.group)
  const result = toInstantiationPage(
    unwrap(
      await listInstantiations({
        client,
        query: {
          group_id: groupId,
          page,
          per_page: perPage,
          // Servers that predate paging only read `limit`.
          limit: perPage,
        },
      })
    )
  )
  const items = result.items
  if (options.json) {
    json(items)
    return
  }
  if (items.length === 0) {
    info(page > 1 ? `No template creations or imports on page ${page}` : 'No template creations or imports yet')
    return
  }
  const columns: TableColumn<InstantiationResponse>[] = [
    { header: 'ID', accessor: (item) => String(item.id) },
    { header: 'Name', key: 'name' },
    {
      header: 'Status',
      accessor: (item) => item.status,
      color: (value) =>
        value === 'succeeded' ? colors.success(value) : value === 'running' || value === 'pending' ? value : colors.error(value),
    },
    {
      header: 'Services',
      accessor: (item) => item.services.map((service) => service.slug).join(', ') || '-',
    },
    { header: 'Started', accessor: (item) => formatRelativeTime(item.created_at), color: (value) => colors.muted(value) },
  ]
  printTable(items, columns, { style: 'minimal' })
  const more = describeMore(result.total, page, perPage, items.length)
  if (more) info(more)
}

/** "Showing 21-40 of 57; next page: --page 3", or nothing when this is all of it. */
export function describeMore(total: number | undefined, page: number, perPage: number, shown: number): string | undefined {
  if (total === undefined) {
    return shown >= perPage ? `Showing the ${shown} most recent; see older ones with --page ${page + 1}` : undefined
  }
  const first = (page - 1) * perPage + 1
  const last = first + shown - 1
  if (page === 1 && last >= total) return undefined
  return last < total
    ? `Showing ${first}-${last} of ${total}; next page: --page ${page + 1}`
    : `Showing ${first}-${last} of ${total}`
}

async function showAction(value: string, options: { wait?: boolean; timeout?: string; json?: boolean }): Promise<void> {
  if (options.json) setQuietMode(true)
  const id = parseId(value)
  const timeoutMs = waitTimeoutMs(options.timeout)
  await requireAuth()
  await setupClient()
  let current = unwrap(await getInstantiation({ client, path: { id } }))
  if (options.wait && !isFinished(current)) current = await waitForInstantiation(id, { timeoutMs })
  report(current, options)
  // Still running is not a failure when the caller chose not to wait.
  if (!options.wait && !isFinished(current)) process.exitCode = 0
}

async function retryAction(value: string, options: { wait?: boolean; timeout?: string; json?: boolean }): Promise<void> {
  if (options.json) setQuietMode(true)
  const id = parseId(value)
  const timeoutMs = waitTimeoutMs(options.timeout)
  await requireAuth()
  await setupClient()
  let current = unwrap(await retryInstantiation({ client, path: { id } }))
  if (options.wait !== false) current = await waitForInstantiation(id, { timeoutMs })
  report(current, options)
  if (options.wait === false) process.exitCode = 0
}
