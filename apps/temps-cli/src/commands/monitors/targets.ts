// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Monitors for targets outside Temps (ADR 108 D3, D4).

import { readFile } from 'node:fs/promises'
import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { createTargetMonitor, getMonitorProbeLocations, updateTargetMonitor } from '../../api/sdk.gen.js'
import type {
  CheckRequest,
  CheckSpec,
  DnsCheckRecordType,
  HttpMethod,
  MonitorLocationResult,
  MonitorResponse,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { json, success, warning, keyValue, newline, header, icons, info } from '../../ui/output.js'

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
  fromNodes?: boolean
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
  fromNodes?: boolean
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

const STATES: Record<string, string> = {
  operational: 'up',
  degraded: 'degraded',
  major_outage: 'down',
}

function locationName(result: { location?: string; name?: string; region?: string | null }): string {
  const name = result.location ?? result.name ?? ''
  return result.region ? `${name} (${result.region})` : name
}

/** One line per location of the latest check of a monitor checked from worker nodes. */
export function locationLines(monitor: MonitorResponse): string[] {
  if (!monitor.probe_from_nodes) return []
  const latest = monitor.location_results
  if (!latest) return ['not checked since its last change']
  return latest.results.map((result: MonitorLocationResult) => {
    const state = result.status ? STATES[result.status] ?? result.status : 'no answer'
    const time = result.response_time_ms != null ? `, ${result.response_time_ms} ms` : ''
    const why = result.message ? ` — ${result.message}` : ''
    return `${locationName(result)}: ${state}${time}${why}`
  })
}

/** The latest per-location answers, for `monitors show`. */
export function printLocations(monitor: MonitorResponse): void {
  const lines = locationLines(monitor)
  if (!lines.length) return
  keyValue('Checked From', 'this server and worker nodes; down when most agree')
  for (const line of lines) keyValue('  Location', line)
}

function printMonitor(monitor: MonitorResponse): void {
  keyValue('ID', monitor.id)
  keyValue('Checks', monitor.check?.target ?? monitor.monitor_url)
  keyValue('Kind', monitor.check_kind)
  if (monitor.check?.header_names.length) {
    keyValue('Headers sent', monitor.check.header_names.join(', '))
  }
  keyValue('Every', `${monitor.check_interval_seconds}s`)
  if (monitor.probe_from_nodes) keyValue('Checked From', 'this server and worker nodes')
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
    .option('--from-nodes', 'Also check from online worker nodes; down only when most locations agree')
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
          probe_from_nodes: options.fromNodes ?? false,
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
    .option('--from-nodes', 'Also check from online worker nodes')
    .option('--no-from-nodes', 'Check from this server only')
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
          probe_from_nodes: options.fromNodes ?? null,
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

  monitors
    .command('locations')
    .description('List where monitors checked from worker nodes are checked from')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(async (options: { projectId: string; json?: boolean }) => {
      await requireAuth()
      await setupClient()
      const locations = await withSpinner('Listing locations...', async () => {
        const { data, error } = await getMonitorProbeLocations({
          client,
          path: { project_id: parseId(options.projectId, '--project-id') },
        })
        if (error || !data) throw new Error(getErrorMessage(error))
        return data
      })
      if (options.json) {
        json(locations)
        return
      }
      newline()
      header(`${icons.info} Probe locations (${locations.locations.length})`)
      for (const location of locations.locations) keyValue('Location', locationName(location))
      if (!locations.configured) {
        newline()
        info(locations.reason ?? 'No worker node is online')
        if (locations.setup_path) info(`Add one in the console: ${locations.setup_path}`)
      }
      newline()
    })
}
