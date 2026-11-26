// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The pieces most API commands share: call one endpoint with a spinner and
 * turn its error into a message that names the operation, print a response
 * as fields or as JSON, and ask before a risky change (refusing without a
 * terminal unless `--yes`).
 */

import { requireAuth } from '../config/store.js'
import { setupClient, getErrorMessage } from './api-client.js'
import { withSpinner } from '../ui/spinner.js'
import { promptConfirm } from '../ui/prompts.js'
import { newline, header, icons, json, success, info, warning, keyValue } from '../ui/output.js'

export interface JsonOption {
  json?: boolean
}

/** Throw an error naming what failed and the API's explanation. */
export function fail(what: string, error: unknown): never {
  throw new Error(`${what}: ${getErrorMessage(error)}`)
}

/** Authenticate and configure the API client. */
export async function connect(): Promise<void> {
  await requireAuth()
  await setupClient()
}

/**
 * Await an SDK call under a spinner and return its data, or throw naming
 * `what` (e.g. "Could not list DNS records"). A call answering 204 returns
 * `undefined`, which is not an error.
 */
export async function call<T>(
  what: string,
  request: () => Promise<{ data?: T; error?: unknown; response?: Response }>,
  spinner = 'Working...',
): Promise<T> {
  return withSpinner(spinner, async () => {
    const { data, error, response } = await request()
    if (error !== undefined && error !== null && !(response && response.ok)) fail(what, error)
    if (response && !response.ok) fail(what, error)
    return data as T
  })
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

/** Print a response object as `key: value` lines; nested values as compact JSON. */
export function printFields(value: unknown, title?: string): void {
  newline()
  if (title) header(`${icons.info} ${title}`)
  if (!isRecord(value)) {
    console.log(value === undefined ? '(no content)' : JSON.stringify(value, null, 2))
    newline()
    return
  }
  for (const [key, field] of Object.entries(value)) {
    keyValue(key, field === null || typeof field !== 'object' ? (field as string | number | boolean | null) : JSON.stringify(field))
  }
  newline()
}

/** Print a list of objects: one JSON line per item, or a header and fields per item. */
export function printList(items: unknown[], title: string, empty: string): void {
  newline()
  header(`${icons.info} ${title} (${items.length})`)
  if (items.length === 0) info(empty)
  for (const item of items) {
    if (isRecord(item)) {
      console.log(
        Object.entries(item)
          .filter(([, v]) => v === null || typeof v !== 'object')
          .map(([k, v]) => `${k}=${v}`)
          .join('  '),
      )
    } else {
      console.log(JSON.stringify(item))
    }
  }
  newline()
}

/** Print `value` as JSON with `--json`, else as fields under `title`. */
export function output(value: unknown, options: JsonOption, title?: string, done?: string): void {
  if (options.json) {
    json(value ?? null)
    return
  }
  if (done) success(done)
  if (value !== undefined && value !== null) printFields(value, title)
}

/**
 * Ask before a risky change, unless `yes`. Without a terminal the prompt
 * refuses (so a script must pass `--yes` deliberately).
 */
export async function confirmRisk(lines: string[], question: string, yes: boolean | undefined): Promise<boolean> {
  if (lines.length === 0 || yes) return true
  for (const line of lines) warning(line)
  const confirmed = await promptConfirm({ message: question, default: false })
  if (!confirmed) info('Cancelled; nothing was changed.')
  return confirmed
}

export { parseIntegerFlag, resolveProjectFlag } from './project-id.js'
