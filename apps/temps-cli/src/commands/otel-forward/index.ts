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
// be added there (see apps/temps-cli/CLAUDE.md). These interfaces are
// hand-maintained to mirror the plugin's serde structs exactly — keep them in
// sync by hand if that shape ever changes.

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

const VENDOR_PRESET_HINT = 'datadog, honeycomb, new_relic, grafana_cloud, generic_otlp'

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
  return getErrorMessage(error)
}

function throwDestinationError(response: Response | undefined, error: unknown): never {
  throw new Error(destinationErrorMessage(response, error))
}

// ============================================================================
// Option parsing helpers (unit tested)
// ============================================================================

/** Collect repeatable `--header` values into an array. */
export function collectHeader(value: string, previous: string[]): string[] {
  return [...previous, value]
}

/**
 * Parse repeated `--header KEY=VALUE` options into an object. Values may
 * contain `=` (only the first `=` splits).
 */
export function parseHeaderPairs(pairs: string[] | undefined): Record<string, string> {
  const out: Record<string, string> = {}
  if (!pairs) return out
  for (const pair of pairs) {
    const idx = pair.indexOf('=')
    if (idx <= 0) {
      throw new Error(`Invalid --header '${pair}': expected KEY=VALUE`)
    }
    const key = pair.slice(0, idx)
    const value = pair.slice(idx + 1)
    out[key] = value
  }
  return out
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
  header?: string[]
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
  header?: string[]
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
    endpoint_url: options.endpointUrl,
  }

  const headers = parseHeaderPairs(options.header)
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
  if (options.endpointUrl !== undefined) body.endpoint_url = options.endpointUrl

  if (options.header && options.header.length > 0) {
    body.headers = parseHeaderPairs(options.header)
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
  newline()
  header(`${icons.info} ${destination.name}`)
  keyValue('ID', destination.id)
  keyValue('Project ID', destination.project_id)
  keyValue('Vendor', destination.vendor_preset)
  keyValue('Endpoint', destination.endpoint_url)
  keyValue('Status', statusBadge(destination.status))
  keyValue('Enabled', destination.enabled ? colors.success('yes') : colors.muted('no'))
  keyValue('Forward traces', destination.forward_traces ? 'yes' : 'no')
  keyValue('Forward metrics', destination.forward_metrics ? 'yes' : 'no')
  keyValue('Forward logs', destination.forward_logs ? 'yes' : 'no')
  keyValue('Allow private network', destination.allow_private_network ? 'yes' : 'no')

  const headerEntries = Object.entries(destination.headers)
  if (headerEntries.length > 0) {
    keyValue('Headers', headerEntries.map(([k, v]) => `${k}=${v}`).join(', '))
  }

  keyValue('Consecutive failures', destination.consecutive_failures)
  keyValue('Last success', destination.last_success_at ? formatDate(destination.last_success_at) : colors.muted('never'))
  keyValue('Last error', destination.last_error_at ? formatDate(destination.last_error_at) : colors.muted('never'))
  if (destination.last_error) {
    keyValue('Last error detail', destination.last_error)
  }
  keyValue('Created', formatDate(destination.created_at))
  keyValue('Updated', formatDate(destination.updated_at))
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
    .option('--header <k=v>', 'HTTP header sent with every export (repeatable)', collectHeader, [] as string[])
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
      '--header <k=v>',
      'HTTP header sent with every export (repeatable); a value of exactly "***" keeps that header\'s existing value',
      collectHeader,
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

  if (options.json) {
    json(result)
    return
  }

  newline()
  header(`${icons.info} OTel Forwarding Destinations for Project ${projectId} (${result.total})`)

  if (result.items.length === 0) {
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

  printTable(result.items, columns, { style: 'minimal' })
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
    json(destination)
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
    json(destination)
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
    json(destination)
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

  if (options.json) {
    json(result)
    return
  }

  newline()
  const statusSuffix = result.http_status !== null ? ` (HTTP ${result.http_status})` : ''
  if (result.success) {
    success(`Test delivery succeeded${statusSuffix}`)
  } else {
    warning(`Test delivery failed${statusSuffix}`)
    if (result.error) {
      warning(result.error)
    }
  }
  newline()
}
