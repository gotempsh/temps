// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Monitors for targets outside Temps (ADR 108 D3, D4).

import { readFile } from 'node:fs/promises'
import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { createTargetMonitor, updateTargetMonitor } from '../../api/sdk.gen.js'
import type {
  CheckRequest,
  CheckSpec,
  DnsCheckRecordType,
  HttpMethod,
  MonitorResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { json, success, warning, keyValue } from '../../ui/output.js'

export const TARGET_KINDS = ['http', 'tcp', 'tls', 'dns'] as const
export type TargetKind = (typeof TARGET_KINDS)[number]
const METHODS: HttpMethod[] = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS']
const RECORDS: DnsCheckRecordType[] = ['A', 'AAAA', 'CNAME', 'MX', 'TXT', 'NS']

export interface CheckOptions {
  method?: string
  expectStatus?: string
  keyword?: string
  keywordAbsent?: boolean
  degradedAfterMs?: string
  warnDays?: string
  record?: string
  expect?: string
}

interface AddOptions extends CheckOptions {
  projectId: string
  kind: string
  target: string
  name: string
  interval?: string
  headerFile?: string
  bodyFile?: string
  json?: boolean
}

interface UpdateOptions extends CheckOptions {
  projectId: string
  monitorId: string
  kind?: string
  target?: string
  name?: string
  interval?: string
  headerFile?: string
  bodyFile?: string
  pause?: boolean
  resume?: boolean
  json?: boolean
}

function list(value: string | undefined): string[] {
  return (value ?? '')
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)
}

function positive(value: string | undefined, flag: string): number | null | string {
  if (value === undefined) return null
  const parsed = Number(value)
  return Number.isInteger(parsed) && parsed > 0 ? parsed : `${flag} takes a whole number above 0`
}

/** The check for `kind` from the flags, or an error message. */
export function checkSpecFromOptions(kind: string, options: CheckOptions): CheckSpec | string {
  if (!(TARGET_KINDS as readonly string[]).includes(kind)) {
    return `--kind must be one of ${TARGET_KINDS.join(', ')}`
  }
  const degraded = positive(options.degradedAfterMs, '--degraded-after-ms')
  if (typeof degraded === 'string') return degraded
  const httpOnly = options.method || options.expectStatus || options.keyword || options.keywordAbsent
  if (kind !== 'http' && httpOnly) {
    return '--method, --expect-status and --keyword only apply to --kind http'
  }
  if (kind !== 'dns' && (options.record || options.expect)) {
    return '--record and --expect only apply to --kind dns'
  }
  if (kind !== 'tls' && options.warnDays) return '--warn-days only applies to --kind tls'
  switch (kind) {
    case 'http': {
      const method = (options.method ?? 'GET').toUpperCase() as HttpMethod
      if (!METHODS.includes(method)) return `--method must be one of ${METHODS.join(', ')}`
      if (options.keywordAbsent && !options.keyword) return '--keyword-absent needs --keyword <text>'
      return {
        kind: 'http',
        method,
        expected_status: list(options.expectStatus),
        keyword: options.keyword ? { text: options.keyword, present: !options.keywordAbsent } : null,
        degraded_after_ms: degraded,
      }
    }
    case 'tcp':
      return { kind: 'tcp', degraded_after_ms: degraded }
    case 'tls': {
      if (degraded !== null) return '--degraded-after-ms applies to http and tcp checks'
      const days = positive(options.warnDays, '--warn-days')
      if (typeof days === 'string') return days
      return { kind: 'tls', warn_days: days }
    }
    default: {
      if (degraded !== null) return '--degraded-after-ms applies to http and tcp checks'
      const record = (options.record ?? 'A').toUpperCase() as DnsCheckRecordType
      if (!RECORDS.includes(record)) return `--record must be one of ${RECORDS.join(', ')}`
      return { kind: 'dns', record_type: record, expected: list(options.expect) }
    }
  }
}

/** `Name: value` lines (blank lines and `#` comments skipped), or an error message. */
export function parseHeaderLines(text: string): CheckRequest['headers'] | string {
  const headers: NonNullable<CheckRequest['headers']> = []
  for (const [index, raw] of text.split(/\r?\n/).entries()) {
    const line = raw.trim()
    if (!line || line.startsWith('#')) continue
    const colon = line.indexOf(':')
    if (colon <= 0) return `line ${index + 1} of the header file is not "Name: value"`
    headers.push({ name: line.slice(0, colon).trim(), value: line.slice(colon + 1).trim() })
  }
  return headers
}

/** The request read from --header-file and --body-file, `null` when neither is given. */
async function requestFromFiles(headerFile?: string, bodyFile?: string): Promise<CheckRequest | null> {
  if (!headerFile && !bodyFile) return null
  const read = (path: string) =>
    readFile(path, 'utf8').catch((error: Error) => {
      throw new Error(`could not read ${path}: ${error.message}`)
    })
  const headers = headerFile ? parseHeaderLines(await read(headerFile)) : []
  if (typeof headers === 'string') throw new Error(headers)
  return { headers, body: bodyFile ? await read(bodyFile) : null }
}

function printMonitor(monitor: MonitorResponse): void {
  keyValue('ID', monitor.id)
  keyValue('Checks', monitor.check?.target ?? monitor.monitor_url)
  keyValue('Kind', monitor.check_kind)
  if (monitor.check?.header_names.length) {
    keyValue('Headers sent', monitor.check.header_names.join(', '))
  }
  keyValue('Every', `${monitor.check_interval_seconds}s`)
}

function parseId(value: string, flag: string): number {
  const id = Number(value)
  if (!Number.isInteger(id) || id <= 0) throw new Error(`${flag} takes a numeric ID`)
  return id
}

const CHECK_FLAGS: Array<[string, string]> = [
  ['--method <method>', 'HTTP method (default GET)'],
  ['--expect-status <codes>', 'Codes or classes counted as up, e.g. 200,204 or 2xx (default 2xx or 3xx)'],
  ['--keyword <text>', 'Text the response body must contain (first 256 KiB; never stored)'],
  ['--degraded-after-ms <ms>', 'Slower than this is degraded (http, tcp)'],
  ['--warn-days <days>', 'Degraded when the certificate expires within this many days (tls, default 14)'],
  ['--record <type>', 'DNS record type: A, AAAA, CNAME, MX, TXT, NS (default A)'],
  ['--expect <values>', 'Comma-separated DNS values; any match is up (default: any answer)'],
  ['--header-file <path>', 'File of "Name: value" request headers; stored encrypted, never shown'],
  ['--body-file <path>', 'File with the request body; stored encrypted'],
]

export function registerTargetMonitorCommands(monitors: Command): void {
  const add = monitors
    .command('add-target')
    .description('Monitor a URL, port, TLS certificate or DNS record outside Temps')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--kind <kind>', 'http, tcp, tls or dns')
    .requiredOption('--target <target>', 'URL (http), host:port (tcp, tls) or host name (dns)')
    .requiredOption('-n, --name <name>', 'Monitor name')
    .option('-i, --interval <seconds>', 'Check interval in seconds (default 60, at least 30)')
    .option('--keyword-absent', 'Fail when --keyword is present instead of missing')
  for (const [flag, description] of CHECK_FLAGS) add.option(flag, description)
  add.option('--json', 'Output in JSON format').action(async (options: AddOptions) => {
    await requireAuth()
    await setupClient()
    const check = checkSpecFromOptions(options.kind, options)
    if (typeof check === 'string') {
      warning(check)
      return
    }
    const interval = positive(options.interval, '--interval')
    if (typeof interval === 'string') {
      warning(interval)
      return
    }
    const projectId = parseId(options.projectId, '--project-id')
    const request = await requestFromFiles(options.headerFile, options.bodyFile)
    if (request && check.kind !== 'http') {
      warning('--header-file and --body-file only apply to --kind http')
      return
    }
    const monitor = await withSpinner('Creating monitor...', async () => {
      const { data, error } = await createTargetMonitor({
        client,
        path: { project_id: projectId },
        body: {
          name: options.name,
          target: options.target,
          check,
          request,
          check_interval_seconds: interval,
        },
      })
      if (error || !data) throw new Error(getErrorMessage(error))
      return data
    })
    if (options.json) {
      json(monitor)
      return
    }
    success(`Monitoring ${monitor.check?.target ?? options.target}; the first check runs within a minute`)
    printMonitor(monitor)
  })

  const update = monitors
    .command('update-target')
    .description('Change a monitor of a target outside Temps; omitted flags keep their value')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--monitor-id <id>', 'Monitor ID')
    .option('--kind <kind>', 'Change the check kind (pass the check flags for it)')
    .option('--target <target>', 'New target')
    .option('-n, --name <name>', 'New name')
    .option('-i, --interval <seconds>', 'Check interval in seconds')
    .option('--keyword-absent', 'Fail when --keyword is present instead of missing')
    .option('--pause', 'Stop checking')
    .option('--resume', 'Start checking again')
  for (const [flag, description] of CHECK_FLAGS) update.option(flag, description)
  update.option('--json', 'Output in JSON format').action(async (options: UpdateOptions) => {
    await requireAuth()
    await setupClient()
    if (options.pause && options.resume) {
      warning('Pass either --pause or --resume, not both')
      return
    }
    const checkFlags = options.method || options.expectStatus || options.keyword || options.keywordAbsent
      || options.degradedAfterMs || options.warnDays || options.record || options.expect
    if (checkFlags && !options.kind) {
      warning('Pass --kind with the check flags: they replace the whole check')
      return
    }
    const check = options.kind ? checkSpecFromOptions(options.kind, options) : null
    if (typeof check === 'string') {
      warning(check)
      return
    }
    const interval = positive(options.interval, '--interval')
    if (typeof interval === 'string') {
      warning(interval)
      return
    }
    const monitor = await withSpinner('Saving monitor...', async () => {
      const { data, error } = await updateTargetMonitor({
        client,
        path: {
          project_id: parseId(options.projectId, '--project-id'),
          monitor_id: parseId(options.monitorId, '--monitor-id'),
        },
        body: {
          name: options.name ?? null,
          target: options.target ?? null,
          check,
          request: await requestFromFiles(options.headerFile, options.bodyFile),
          check_interval_seconds: interval,
          is_active: options.pause ? false : options.resume ? true : null,
        },
      })
      if (error || !data) throw new Error(getErrorMessage(error))
      return data
    })
    if (options.json) {
      json(monitor)
      return
    }
    success(`Saved monitor ${monitor.id}${monitor.is_active ? '' : ' (paused)'}`)
    printMonitor(monitor)
  })
}
