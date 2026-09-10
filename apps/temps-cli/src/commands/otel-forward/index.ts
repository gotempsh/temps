// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  newline, header, icons, json, colors, success, info, warning,
  keyValue, formatDate,
} from '../../ui/output.js'

// ============================================================================
// Hand-written request/response shapes
// ============================================================================
//
// This feature is implemented by a plugin crate that is not part of this
// repository, so its schema does not exist in `openapi.json` and must never
// be added there (see "Regenerating the OpenAPI clients" in the root
// CLAUDE.md). These interfaces are hand-maintained to mirror the plugin's
// serde structs exactly — keep them in sync by hand if that shape ever
// changes.

export interface CreateDestinationBody {
  project_id: number
  name: string
  vendor_preset: string
  endpoint_url: string
  headers?: Record<string, string>
  forward_traces?: boolean
  forward_metrics?: boolean
  forward_logs?: boolean
  enabled?: boolean
  allow_private_network?: boolean
}

export interface UpdateDestinationBody {
  name?: string
  vendor_preset?: string
  endpoint_url?: string
  headers?: Record<string, string>
  forward_traces?: boolean
  forward_metrics?: boolean
  forward_logs?: boolean
  enabled?: boolean
  allow_private_network?: boolean
}

export type DestinationStatus = 'healthy' | 'degraded' | 'failing' | 'disabled' | 'never_delivered'

export interface DestinationResponse {
  id: number
  project_id: number
  name: string
  vendor_preset: string
  endpoint_url: string
  headers: Record<string, string>
  forward_traces: boolean
  forward_metrics: boolean
  forward_logs: boolean
  enabled: boolean
  allow_private_network: boolean
  last_success_at: string | null
  last_error_at: string | null
  last_error: string | null
  consecutive_failures: number
  status: DestinationStatus
  created_at: string
  updated_at: string
}

export interface DestinationListResponse {
  items: DestinationResponse[]
  total: number
}

export interface TestDeliveryResponse {
  success: boolean
  http_status: number | null
  error: string | null
}

// Instance-level default destinations. Same shape as a project destination
// minus project_id — the relay engine falls back to these for any project
// with zero enabled destinations of its own.

export interface CreateInstanceDefaultBody {
  name: string
  vendor_preset: string
  endpoint_url: string
  headers?: Record<string, string>
  forward_traces?: boolean
  forward_metrics?: boolean
  forward_logs?: boolean
  enabled?: boolean
  allow_private_network?: boolean
}

export interface UpdateInstanceDefaultBody {
  name?: string
  vendor_preset?: string
  endpoint_url?: string
  headers?: Record<string, string>
  forward_traces?: boolean
  forward_metrics?: boolean
  forward_logs?: boolean
  enabled?: boolean
  allow_private_network?: boolean
}

export interface InstanceDefaultResponse {
  id: number
  name: string
  vendor_preset: string
  endpoint_url: string
  headers: Record<string, string>
  forward_traces: boolean
  forward_metrics: boolean
  forward_logs: boolean
  enabled: boolean
  allow_private_network: boolean
  last_success_at: string | null
  last_error_at: string | null
  last_error: string | null
  consecutive_failures: number
  status: DestinationStatus
  created_at: string
  updated_at: string
}

export interface InstanceDefaultListResponse {
  items: InstanceDefaultResponse[]
  total: number
}

const VENDOR_PRESET_HINT = 'datadog, honeycomb, new_relic, grafana_cloud, generic_otlp'

/** Mask every returned header value. Header names are not a reliable signal
 * of sensitivity (`x-honeycomb-team`, for example, is an API credential), so
 * output must be fail-closed for both human-readable and JSON modes.
 */
export function maskHeaders(headers: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.keys(headers).map((name) => [name, '***']))
}

/** Remove URL credentials and redact every query value before display. */
export function sanitizeUrlForOutput(value: string): string {
  try {
    const url = new URL(value)
    url.username = ''
    url.password = ''
    for (const name of [...url.searchParams.keys()]) {
      url.searchParams.set(name, '***')
    }
    url.hash = ''
    return url.toString()
  } catch {
    return '[invalid URL redacted]'
  }
}

/** Sanitize URLs embedded in server and transport error strings. */
export function sanitizeTextForOutput(value: string): string {
  return value.replace(/https?:\/\/[^\s"'<>]+/gi, (url) => sanitizeUrlForOutput(url))
}

export function sanitizeDestinationForOutput<T extends DestinationResponse | InstanceDefaultResponse>(
  destination: T
): T {
  return {
    ...destination,
    endpoint_url: sanitizeUrlForOutput(destination.endpoint_url),
    headers: maskHeaders(destination.headers),
    last_error: destination.last_error ? sanitizeTextForOutput(destination.last_error) : null,
  }
}

function sanitizeTestDeliveryForOutput(result: TestDeliveryResponse): TestDeliveryResponse {
  return {
    ...result,
    error: result.error ? sanitizeTextForOutput(result.error) : null,
  }
}

// ============================================================================
// 404 handling
// ============================================================================
//
// This routes through a plugin that may not be installed/enabled on a given
// server. On a plain OSS instance (or one without this plugin), every one of
// these routes 404s because the route simply doesn't exist — that's a
// deliberate case to handle clearly rather than surface as a generic error.

const OTEL_FORWARD_UNAVAILABLE_MESSAGE =
  "OTel forwarding isn't available on this server — the endpoint returned 404. " +
  'This feature may require a plugin that isn\'t installed on this Temps instance.'

function destinationErrorMessage(response: Response | undefined, error: unknown): string {
  if (response?.status === 404) {
    return OTEL_FORWARD_UNAVAILABLE_MESSAGE
  }
  return sanitizeTextForOutput(getErrorMessage(error))
}

function throwDestinationError(response: Response | undefined, error: unknown): never {
  throw new Error(destinationErrorMessage(response, error))
}

// ============================================================================
// Option parsing helpers (unit tested)
// ============================================================================

/** Collect repeatable `--header-env` references into an array. */
export function collectHeaderEnv(value: string, previous: string[]): string[] {
  return [...previous, value]
}

/**
 * Resolve repeated `--header-env KEY=ENV_VAR` options without putting the
 * credential itself in argv or shell history.
 */
export function parseHeaderEnvPairs(
  pairs: string[] | undefined,
  environment: NodeJS.ProcessEnv = process.env
): Record<string, string> {
  const out: Record<string, string> = {}
  if (!pairs) return out
  for (const pair of pairs) {
    const idx = pair.indexOf('=')
    if (idx <= 0 || idx === pair.length - 1) {
      throw new Error("Invalid --header-env: expected KEY=ENV_VAR")
    }
    const key = pair.slice(0, idx)
    const environmentName = pair.slice(idx + 1)
    const value = environment[environmentName]
    if (value === undefined) {
      throw new Error(`Environment variable ${environmentName} is not set`)
    }
    out[key] = value
  }
  return out
}

/** Reject endpoint URL components that commonly carry credentials and would
 * otherwise be exposed through argv, persisted configuration, or errors.
 */
export function validateEndpointUrl(value: string): string {
  let url: URL
  try {
    url = new URL(value)
  } catch {
    throw new Error('Invalid --endpoint-url: expected an absolute HTTP(S) URL')
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    throw new Error('Invalid --endpoint-url: only HTTP(S) URLs are supported')
  }
  if (url.username || url.password) {
    throw new Error('Invalid --endpoint-url: URL credentials are not allowed; use --header-env')
  }
  if (url.search) {
    throw new Error('Invalid --endpoint-url: query parameters are not allowed; use --header-env')
  }
  if (url.hash) {
    throw new Error('Invalid --endpoint-url: fragments are not allowed')
  }
  return value
}

/**
 * Resolve the tri-state `enabled` flag from the separate `--enabled` /
 * `--disabled` commander options. Returns `undefined` when neither was
 * passed, so callers can tell "not specified" apart from "explicitly false".
 */
export function resolveEnabledFlag(options: { enabled?: boolean; disabled?: boolean }): boolean | undefined {
  if (options.disabled) return false
  if (options.enabled) return true
  return undefined
}

interface CreateOptions {
  projectId: string
  name: string
  vendor: string
  endpointUrl: string
  headerEnv?: string[]
  traces?: boolean
  metrics?: boolean
  logs?: boolean
  enabled?: boolean
  disabled?: boolean
  allowPrivateNetwork?: boolean
  json?: boolean
}

interface UpdateOptions {
  name?: string
  vendor?: string
  endpointUrl?: string
  headerEnv?: string[]
  traces?: boolean
  metrics?: boolean
  logs?: boolean
  enabled?: boolean
  disabled?: boolean
  allowPrivateNetwork?: boolean
  json?: boolean
}

/** Build the POST body from `create`'s options. Omitted flags are left out entirely so the server applies its own defaults. */
export function buildCreateDestinationBody(projectId: number, options: CreateOptions): CreateDestinationBody {
  const body: CreateDestinationBody = {
    project_id: projectId,
    name: options.name,
    vendor_preset: options.vendor,
    endpoint_url: validateEndpointUrl(options.endpointUrl),
  }

  const headers = parseHeaderEnvPairs(options.headerEnv)
  if (Object.keys(headers).length > 0) {
    body.headers = headers
  }

  if (options.traces !== undefined) body.forward_traces = options.traces
  if (options.metrics !== undefined) body.forward_metrics = options.metrics
  if (options.logs !== undefined) body.forward_logs = options.logs

  const enabled = resolveEnabledFlag(options)
  if (enabled !== undefined) body.enabled = enabled

  if (options.allowPrivateNetwork) body.allow_private_network = true

  return body
}

/** Build the PATCH body from `update`'s options — only fields the user actually passed are included. */
export function buildUpdateDestinationBody(options: UpdateOptions): UpdateDestinationBody {
  const body: UpdateDestinationBody = {}

  if (options.name !== undefined) body.name = options.name
  if (options.vendor !== undefined) body.vendor_preset = options.vendor
  if (options.endpointUrl !== undefined) body.endpoint_url = validateEndpointUrl(options.endpointUrl)

  if (options.headerEnv && options.headerEnv.length > 0) {
    body.headers = parseHeaderEnvPairs(options.headerEnv)
  }

  if (options.traces !== undefined) body.forward_traces = options.traces
  if (options.metrics !== undefined) body.forward_metrics = options.metrics
  if (options.logs !== undefined) body.forward_logs = options.logs

  const enabled = resolveEnabledFlag(options)
  if (enabled !== undefined) body.enabled = enabled

  if (options.allowPrivateNetwork !== undefined) body.allow_private_network = options.allowPrivateNetwork

  return body
}

interface CreateInstanceDefaultOptions {
  name: string
  vendor: string
  endpointUrl: string
  headerEnv?: string[]
  traces?: boolean
  metrics?: boolean
  logs?: boolean
  enabled?: boolean
  disabled?: boolean
  allowPrivateNetwork?: boolean
  json?: boolean
}

interface UpdateInstanceDefaultOptions {
  name?: string
  vendor?: string
  endpointUrl?: string
  headerEnv?: string[]
  traces?: boolean
  metrics?: boolean
  logs?: boolean
  enabled?: boolean
  disabled?: boolean
  allowPrivateNetwork?: boolean
  json?: boolean
}

/** Build the POST body from `instance-default create`'s options. Omitted flags are left out entirely so the server applies its own defaults. */
export function buildCreateInstanceDefaultBody(options: CreateInstanceDefaultOptions): CreateInstanceDefaultBody {
  const body: CreateInstanceDefaultBody = {
    name: options.name,
    vendor_preset: options.vendor,
    endpoint_url: validateEndpointUrl(options.endpointUrl),
  }

  const headers = parseHeaderEnvPairs(options.headerEnv)
  if (Object.keys(headers).length > 0) {
    body.headers = headers
  }

  if (options.traces !== undefined) body.forward_traces = options.traces
  if (options.metrics !== undefined) body.forward_metrics = options.metrics
  if (options.logs !== undefined) body.forward_logs = options.logs

  const enabled = resolveEnabledFlag(options)
  if (enabled !== undefined) body.enabled = enabled

  if (options.allowPrivateNetwork) body.allow_private_network = true

  return body
}

/** Build the PATCH body from `instance-default update`'s options — only fields the user actually passed are included. */
export function buildUpdateInstanceDefaultBody(options: UpdateInstanceDefaultOptions): UpdateInstanceDefaultBody {
  const body: UpdateInstanceDefaultBody = {}

  if (options.name !== undefined) body.name = options.name
  if (options.vendor !== undefined) body.vendor_preset = options.vendor
  if (options.endpointUrl !== undefined) body.endpoint_url = validateEndpointUrl(options.endpointUrl)

  if (options.headerEnv && options.headerEnv.length > 0) {
    body.headers = parseHeaderEnvPairs(options.headerEnv)
  }

  if (options.traces !== undefined) body.forward_traces = options.traces
  if (options.metrics !== undefined) body.forward_metrics = options.metrics
  if (options.logs !== undefined) body.forward_logs = options.logs

  const enabled = resolveEnabledFlag(options)
  if (enabled !== undefined) body.enabled = enabled

  if (options.allowPrivateNetwork !== undefined) body.allow_private_network = options.allowPrivateNetwork

  return body
}

// ============================================================================
// Display helpers
// ============================================================================

function printDestinationDetails(destination: DestinationResponse): void {
  const safeDestination = sanitizeDestinationForOutput(destination)
  newline()
  header(`${icons.info} ${safeDestination.name}`)
  keyValue('ID', safeDestination.id)
  keyValue('Project ID', safeDestination.project_id)
  keyValue('Vendor', safeDestination.vendor_preset)
  keyValue('Endpoint', safeDestination.endpoint_url)
  keyValue('Status', statusBadge(safeDestination.status))
  keyValue('Enabled', safeDestination.enabled ? colors.success('yes') : colors.muted('no'))
  keyValue('Forward traces', safeDestination.forward_traces ? 'yes' : 'no')
  keyValue('Forward metrics', safeDestination.forward_metrics ? 'yes' : 'no')
  keyValue('Forward logs', safeDestination.forward_logs ? 'yes' : 'no')
  keyValue('Allow private network', safeDestination.allow_private_network ? 'yes' : 'no')

  const headerEntries = Object.entries(safeDestination.headers)
  if (headerEntries.length > 0) {
    keyValue('Headers', headerEntries.map(([k, v]) => `${k}=${v}`).join(', '))
  }

  keyValue('Consecutive failures', safeDestination.consecutive_failures)
  keyValue('Last success', safeDestination.last_success_at ? formatDate(safeDestination.last_success_at) : colors.muted('never'))
  keyValue('Last error', safeDestination.last_error_at ? formatDate(safeDestination.last_error_at) : colors.muted('never'))
  if (safeDestination.last_error) {
    keyValue('Last error detail', safeDestination.last_error)
  }
  keyValue('Created', formatDate(safeDestination.created_at))
  keyValue('Updated', formatDate(safeDestination.updated_at))
  newline()
}

function printInstanceDefaultDetails(instanceDefault: InstanceDefaultResponse): void {
  const safeInstanceDefault = sanitizeDestinationForOutput(instanceDefault)
  newline()
  header(`${icons.info} ${safeInstanceDefault.name} (instance default)`)
  keyValue('ID', safeInstanceDefault.id)
  keyValue('Vendor', safeInstanceDefault.vendor_preset)
  keyValue('Endpoint', safeInstanceDefault.endpoint_url)
  keyValue('Status', statusBadge(safeInstanceDefault.status))
  keyValue('Enabled', safeInstanceDefault.enabled ? colors.success('yes') : colors.muted('no'))
  keyValue('Forward traces', safeInstanceDefault.forward_traces ? 'yes' : 'no')
  keyValue('Forward metrics', safeInstanceDefault.forward_metrics ? 'yes' : 'no')
  keyValue('Forward logs', safeInstanceDefault.forward_logs ? 'yes' : 'no')
  keyValue('Allow private network', safeInstanceDefault.allow_private_network ? 'yes' : 'no')

  const headerEntries = Object.entries(safeInstanceDefault.headers)
  if (headerEntries.length > 0) {
    keyValue('Headers', headerEntries.map(([k, v]) => `${k}=${v}`).join(', '))
  }

  keyValue('Consecutive failures', safeInstanceDefault.consecutive_failures)
  keyValue('Last success', safeInstanceDefault.last_success_at ? formatDate(safeInstanceDefault.last_success_at) : colors.muted('never'))
  keyValue('Last error', safeInstanceDefault.last_error_at ? formatDate(safeInstanceDefault.last_error_at) : colors.muted('never'))
  if (safeInstanceDefault.last_error) {
    keyValue('Last error detail', safeInstanceDefault.last_error)
  }
  keyValue('Created', formatDate(safeInstanceDefault.created_at))
  keyValue('Updated', formatDate(safeInstanceDefault.updated_at))
  newline()
}

// ============================================================================
// Commander wiring
// ============================================================================

export function registerOtelForwardCommands(program: Command): void {
  const otelForward = program
    .command('otel-forward')
    .description('Manage OTel forwarding destinations that relay ingested traces, metrics, and logs to an external OTLP-compatible collector')

  otelForward
    .command('list')
    .alias('ls')
    .description('List OTel forwarding destinations for a project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listDestinationsAction)

  otelForward
    .command('create')
    .description('Create a new OTel forwarding destination')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--name <name>', 'Destination name')
    .requiredOption('--vendor <preset>', `Vendor preset (${VENDOR_PRESET_HINT})`)
    .requiredOption('--endpoint-url <url>', 'OTLP-compatible collector endpoint URL')
    .option('--header-env <k=env>', 'HTTP header sourced from an environment variable (repeatable)', collectHeaderEnv, [] as string[])
    .option('--traces', 'Forward traces (default: true)')
    .option('--no-traces', 'Do not forward traces')
    .option('--metrics', 'Forward metrics (default: true)')
    .option('--no-metrics', 'Do not forward metrics')
    .option('--logs', 'Forward logs (default: true)')
    .option('--no-logs', 'Do not forward logs')
    .option('--enabled', 'Create the destination enabled (default)')
    .option('--disabled', 'Create the destination disabled')
    .option('--allow-private-network', 'Allow the endpoint URL to resolve to private/loopback/link-local IPs')
    .option('--json', 'Output in JSON format')
    .action(createDestinationAction)

  otelForward
    .command('show <id>')
    .description('Show OTel forwarding destination details')
    .option('--json', 'Output in JSON format')
    .action(showDestinationAction)

  otelForward
    .command('update <id>')
    .description('Update an OTel forwarding destination')
    .option('--name <name>', 'Destination name')
    .option('--vendor <preset>', `Vendor preset (${VENDOR_PRESET_HINT})`)
    .option('--endpoint-url <url>', 'OTLP-compatible collector endpoint URL')
    .option(
      '--header-env <k=env>',
      'HTTP header sourced from an environment variable (repeatable)',
      collectHeaderEnv,
      [] as string[]
    )
    .option('--traces', 'Forward traces')
    .option('--no-traces', 'Do not forward traces')
    .option('--metrics', 'Forward metrics')
    .option('--no-metrics', 'Do not forward metrics')
    .option('--logs', 'Forward logs')
    .option('--no-logs', 'Do not forward logs')
    .option('--enabled', 'Enable the destination')
    .option('--disabled', 'Disable the destination')
    .option('--allow-private-network', 'Allow the endpoint URL to resolve to private/loopback/link-local IPs')
    .option('--no-allow-private-network', 'Disallow private/loopback/link-local endpoint URLs')
    .option('--json', 'Output in JSON format')
    .action(updateDestinationAction)

  otelForward
    .command('remove <id>')
    .description('Remove an OTel forwarding destination')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeDestinationAction)

  otelForward
    .command('test <id>')
    .description('Send a test delivery to an OTel forwarding destination')
    .option('--json', 'Output in JSON format')
    .action(testDestinationAction)

  const instanceDefault = otelForward
    .command('instance-default')
    .description(
      'Manage instance-wide default forwarding destinations — applied automatically to any ' +
        'project with zero enabled destinations of its own. As soon as a project has one of its ' +
        'own destinations, instance defaults stop applying to that project.'
    )

  instanceDefault
    .command('list')
    .alias('ls')
    .description('List instance-wide default forwarding destinations')
    .option('--json', 'Output in JSON format')
    .action(listInstanceDefaultsAction)

  instanceDefault
    .command('create')
    .description('Create a new instance-wide default forwarding destination')
    .requiredOption('--name <name>', 'Destination name')
    .requiredOption('--vendor <preset>', `Vendor preset (${VENDOR_PRESET_HINT})`)
    .requiredOption('--endpoint-url <url>', 'OTLP-compatible collector endpoint URL')
    .option('--header-env <k=env>', 'HTTP header sourced from an environment variable (repeatable)', collectHeaderEnv, [] as string[])
    .option('--traces', 'Forward traces (default: true)')
    .option('--no-traces', 'Do not forward traces')
    .option('--metrics', 'Forward metrics (default: true)')
    .option('--no-metrics', 'Do not forward metrics')
    .option('--logs', 'Forward logs (default: true)')
    .option('--no-logs', 'Do not forward logs')
    .option('--enabled', 'Create the destination enabled (default)')
    .option('--disabled', 'Create the destination disabled')
    .option('--allow-private-network', 'Allow the endpoint URL to resolve to private/loopback/link-local IPs')
    .option('--json', 'Output in JSON format')
    .action(createInstanceDefaultAction)

  instanceDefault
    .command('show <id>')
    .description('Show instance default destination details')
    .option('--json', 'Output in JSON format')
    .action(showInstanceDefaultAction)

  instanceDefault
    .command('update <id>')
    .description('Update an instance default forwarding destination')
    .option('--name <name>', 'Destination name')
    .option('--vendor <preset>', `Vendor preset (${VENDOR_PRESET_HINT})`)
    .option('--endpoint-url <url>', 'OTLP-compatible collector endpoint URL')
    .option(
      '--header-env <k=env>',
      'HTTP header sourced from an environment variable (repeatable)',
      collectHeaderEnv,
      [] as string[]
    )
    .option('--traces', 'Forward traces')
    .option('--no-traces', 'Do not forward traces')
    .option('--metrics', 'Forward metrics')
    .option('--no-metrics', 'Do not forward metrics')
    .option('--logs', 'Forward logs')
    .option('--no-logs', 'Do not forward logs')
    .option('--enabled', 'Enable the destination')
    .option('--disabled', 'Disable the destination')
    .option('--allow-private-network', 'Allow the endpoint URL to resolve to private/loopback/link-local IPs')
    .option('--no-allow-private-network', 'Disallow private/loopback/link-local endpoint URLs')
    .option('--json', 'Output in JSON format')
    .action(updateInstanceDefaultAction)

  instanceDefault
    .command('remove <id>')
    .description('Remove an instance default forwarding destination')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .action(removeInstanceDefaultAction)

  instanceDefault
    .command('test <id>')
    .description('Send a test delivery to an instance default forwarding destination')
    .option('--json', 'Output in JSON format')
    .action(testInstanceDefaultAction)
}

// ============================================================================
// Actions
// ============================================================================

async function listDestinationsAction(options: { projectId: string; json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  const result = await withSpinner('Fetching OTel forwarding destinations...', async () => {
    const { data, error, response } = await client.get<DestinationListResponse, ProblemDetails>({
      url: 'ee/otel-forward/destinations',
      query: { project_id: projectId },
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })
  const safeResult: DestinationListResponse = {
    ...result,
    items: result.items.map(sanitizeDestinationForOutput),
  }

  if (options.json) {
    json(safeResult)
    return
  }

  newline()
  header(`${icons.info} OTel Forwarding Destinations for Project ${projectId} (${safeResult.total})`)

  if (safeResult.items.length === 0) {
    info('No OTel forwarding destinations configured')
    info('Run: temps otel-forward create --project-id ' + projectId + ' --name <name> --vendor <preset> --endpoint-url <url>')
    newline()
    return
  }

  const columns: TableColumn<DestinationResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Vendor', key: 'vendor_preset' },
    { header: 'Endpoint', key: 'endpoint_url', color: (v) => colors.muted(v) },
    { header: 'Status', accessor: (d) => d.status, color: (v) => statusBadge(v) },
    {
      header: 'Failures',
      accessor: (d) => d.consecutive_failures.toString(),
      color: (v) => (parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v)),
    },
  ]

  printTable(safeResult.items, columns, { style: 'minimal' })
  newline()
}

async function createDestinationAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return
  }

  let body: CreateDestinationBody
  try {
    body = buildCreateDestinationBody(projectId, options)
  } catch (err) {
    warning(getErrorMessage(err))
    return
  }

  const destination = await withSpinner(`Creating OTel forwarding destination "${options.name}"...`, async () => {
    const { data, error, response } = await client.post<DestinationResponse, ProblemDetails>({
      url: 'ee/otel-forward/destinations',
      body,
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })

  if (options.json) {
    json(sanitizeDestinationForOutput(destination))
    return
  }

  success(`OTel forwarding destination "${destination.name}" created`)
  printDestinationDetails(destination)
}

async function showDestinationAction(id: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const destinationId = parseInt(id, 10)
  if (isNaN(destinationId)) {
    warning('Invalid destination ID')
    return
  }

  const destination = await withSpinner('Fetching OTel forwarding destination...', async () => {
    const { data, error, response } = await client.get<DestinationResponse, ProblemDetails>({
      url: 'ee/otel-forward/destinations/{id}',
      path: { id: destinationId },
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })

  if (options.json) {
    json(sanitizeDestinationForOutput(destination))
    return
  }

  printDestinationDetails(destination)
}

async function updateDestinationAction(id: string, options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const destinationId = parseInt(id, 10)
  if (isNaN(destinationId)) {
    warning('Invalid destination ID')
    return
  }

  let body: UpdateDestinationBody
  try {
    body = buildUpdateDestinationBody(options)
  } catch (err) {
    warning(getErrorMessage(err))
    return
  }

  if (Object.keys(body).length === 0) {
    warning('No fields to update — pass at least one option (e.g. --name, --endpoint-url, --enabled)')
    return
  }

  const destination = await withSpinner('Updating OTel forwarding destination...', async () => {
    const { data, error, response } = await client.patch<DestinationResponse, ProblemDetails>({
      url: 'ee/otel-forward/destinations/{id}',
      path: { id: destinationId },
      body,
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })

  if (options.json) {
    json(sanitizeDestinationForOutput(destination))
    return
  }

  success(`OTel forwarding destination #${destinationId} updated`)
  printDestinationDetails(destination)
}

async function removeDestinationAction(id: string, options: { force?: boolean; yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const destinationId = parseInt(id, 10)
  if (isNaN(destinationId)) {
    warning('Invalid destination ID')
    return
  }

  const { data: destination, error: getError, response: getResponse } = await client.get<DestinationResponse, ProblemDetails>({
    url: 'ee/otel-forward/destinations/{id}',
    path: { id: destinationId },
  })

  if (getError || !destination) {
    warning(destinationErrorMessage(getResponse, getError))
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message: `Remove OTel forwarding destination "${destination.name}" (${destination.vendor_preset})?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing OTel forwarding destination...', async () => {
    const { error, response } = await client.delete<void, ProblemDetails>({
      url: 'ee/otel-forward/destinations/{id}',
      path: { id: destinationId },
    })
    if (error) {
      throwDestinationError(response, error)
    }
  })

  success('OTel forwarding destination removed')
}

async function testDestinationAction(id: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const destinationId = parseInt(id, 10)
  if (isNaN(destinationId)) {
    warning('Invalid destination ID')
    return
  }

  const result = await withSpinner('Sending test delivery...', async () => {
    const { data, error, response } = await client.post<TestDeliveryResponse, ProblemDetails>({
      url: 'ee/otel-forward/destinations/{id}/test',
      path: { id: destinationId },
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })
  const safeResult = sanitizeTestDeliveryForOutput(result)

  if (options.json) {
    json(safeResult)
    return
  }

  newline()
  const statusSuffix = safeResult.http_status !== null ? ` (HTTP ${safeResult.http_status})` : ''
  if (safeResult.success) {
    success(`Test delivery succeeded${statusSuffix}`)
  } else {
    warning(`Test delivery failed${statusSuffix}`)
    if (safeResult.error) {
      warning(safeResult.error)
    }
  }
  newline()
}

// ============================================================================
// Instance default actions
// ============================================================================

async function listInstanceDefaultsAction(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const result = await withSpinner('Fetching instance default forwarding destinations...', async () => {
    const { data, error, response } = await client.get<InstanceDefaultListResponse, ProblemDetails>({
      url: 'ee/otel-forward/instance-defaults',
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })
  const safeResult: InstanceDefaultListResponse = {
    ...result,
    items: result.items.map(sanitizeDestinationForOutput),
  }

  if (options.json) {
    json(safeResult)
    return
  }

  newline()
  header(`${icons.info} Instance Default Forwarding Destinations (${safeResult.total})`)

  if (safeResult.items.length === 0) {
    info('No instance default forwarding destinations configured')
    info(
      'Run: temps otel-forward instance-default create --name <name> --vendor <preset> --endpoint-url <url>'
    )
    newline()
    return
  }

  const columns: TableColumn<InstanceDefaultResponse>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    { header: 'Vendor', key: 'vendor_preset' },
    { header: 'Endpoint', key: 'endpoint_url', color: (v) => colors.muted(v) },
    { header: 'Status', accessor: (d) => d.status, color: (v) => statusBadge(v) },
    {
      header: 'Failures',
      accessor: (d) => d.consecutive_failures.toString(),
      color: (v) => (parseInt(v, 10) > 0 ? colors.error(v) : colors.muted(v)),
    },
  ]

  printTable(safeResult.items, columns, { style: 'minimal' })
  newline()
}

async function createInstanceDefaultAction(options: CreateInstanceDefaultOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  let body: CreateInstanceDefaultBody
  try {
    body = buildCreateInstanceDefaultBody(options)
  } catch (err) {
    warning(getErrorMessage(err))
    return
  }

  const instanceDefault = await withSpinner(
    `Creating instance default forwarding destination "${options.name}"...`,
    async () => {
      const { data, error, response } = await client.post<InstanceDefaultResponse, ProblemDetails>({
        url: 'ee/otel-forward/instance-defaults',
        body,
      })
      if (error || !data) {
        throwDestinationError(response, error)
      }
      return data
    }
  )

  if (options.json) {
    json(sanitizeDestinationForOutput(instanceDefault))
    return
  }

  success(`Instance default forwarding destination "${instanceDefault.name}" created`)
  printInstanceDefaultDetails(instanceDefault)
}

async function showInstanceDefaultAction(id: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const instanceDefaultId = parseInt(id, 10)
  if (isNaN(instanceDefaultId)) {
    warning('Invalid instance default ID')
    return
  }

  const instanceDefault = await withSpinner('Fetching instance default forwarding destination...', async () => {
    const { data, error, response } = await client.get<InstanceDefaultResponse, ProblemDetails>({
      url: 'ee/otel-forward/instance-defaults/{id}',
      path: { id: instanceDefaultId },
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })

  if (options.json) {
    json(sanitizeDestinationForOutput(instanceDefault))
    return
  }

  printInstanceDefaultDetails(instanceDefault)
}

async function updateInstanceDefaultAction(id: string, options: UpdateInstanceDefaultOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const instanceDefaultId = parseInt(id, 10)
  if (isNaN(instanceDefaultId)) {
    warning('Invalid instance default ID')
    return
  }

  let body: UpdateInstanceDefaultBody
  try {
    body = buildUpdateInstanceDefaultBody(options)
  } catch (err) {
    warning(getErrorMessage(err))
    return
  }

  if (Object.keys(body).length === 0) {
    warning('No fields to update — pass at least one option (e.g. --name, --endpoint-url, --enabled)')
    return
  }

  const instanceDefault = await withSpinner('Updating instance default forwarding destination...', async () => {
    const { data, error, response } = await client.patch<InstanceDefaultResponse, ProblemDetails>({
      url: 'ee/otel-forward/instance-defaults/{id}',
      path: { id: instanceDefaultId },
      body,
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })

  if (options.json) {
    json(sanitizeDestinationForOutput(instanceDefault))
    return
  }

  success(`Instance default forwarding destination #${instanceDefaultId} updated`)
  printInstanceDefaultDetails(instanceDefault)
}

async function removeInstanceDefaultAction(id: string, options: { force?: boolean; yes?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const instanceDefaultId = parseInt(id, 10)
  if (isNaN(instanceDefaultId)) {
    warning('Invalid instance default ID')
    return
  }

  const { data: instanceDefault, error: getError, response: getResponse } = await client.get<
    InstanceDefaultResponse,
    ProblemDetails
  >({
    url: 'ee/otel-forward/instance-defaults/{id}',
    path: { id: instanceDefaultId },
  })

  if (getError || !instanceDefault) {
    warning(destinationErrorMessage(getResponse, getError))
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    const confirmed = await promptConfirm({
      message:
        `Remove instance default "${instanceDefault.name}" (${instanceDefault.vendor_preset})? ` +
        'Any project with no destinations of its own will stop receiving forwarded telemetry.',
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Removing instance default forwarding destination...', async () => {
    const { error, response } = await client.delete<void, ProblemDetails>({
      url: 'ee/otel-forward/instance-defaults/{id}',
      path: { id: instanceDefaultId },
    })
    if (error) {
      throwDestinationError(response, error)
    }
  })

  success('Instance default forwarding destination removed')
}

async function testInstanceDefaultAction(id: string, options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const instanceDefaultId = parseInt(id, 10)
  if (isNaN(instanceDefaultId)) {
    warning('Invalid instance default ID')
    return
  }

  const result = await withSpinner('Sending test delivery...', async () => {
    const { data, error, response } = await client.post<TestDeliveryResponse, ProblemDetails>({
      url: 'ee/otel-forward/instance-defaults/{id}/test',
      path: { id: instanceDefaultId },
    })
    if (error || !data) {
      throwDestinationError(response, error)
    }
    return data
  })
  const safeResult = sanitizeTestDeliveryForOutput(result)

  if (options.json) {
    json(safeResult)
    return
  }

  newline()
  const statusSuffix = safeResult.http_status !== null ? ` (HTTP ${safeResult.http_status})` : ''
  if (safeResult.success) {
    success(`Test delivery succeeded${statusSuffix}`)
  } else {
    warning(`Test delivery failed${statusSuffix}`)
    if (safeResult.error) {
      warning(safeResult.error)
    }
  }
  newline()
}
