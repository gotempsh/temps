// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps cloud telemetry` — where a project's spans are written (ADR-041).
 *
 * ============================================================================
 * Hand-written request/response shapes
 * ============================================================================
 *
 * These routes live in the OTel plugin under `/otel/cloud-telemetry/*` and are
 * called through the shared `client` object rather than the generated SDK. The
 * interfaces below mirror the serde structs in
 * `crates/temps-otel/src/handlers/cloud_telemetry_handler.rs` and must be kept
 * in sync by hand if that shape changes.
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import { getProjectBySlug } from '../../api/sdk.gen.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import {
  colors,
  formatDate,
  header,
  icons,
  info,
  json,
  keyValue,
  newline,
  success,
  warning,
} from '../../ui/output.js'
import { registerCloudTelemetryBulkCommands } from './bulk-activation.js'

// ── Shapes ─────────────────────────────────────────────────────────────────

export type CloudTelemetryWriteMode = 'local' | 'cloud'
export type CloudTelemetryFidelity = 'metered' | 'queryable'

export type TelemetryWriteIntervalReason =
  | 'operator'
  | 'cloud_disconnected'
  | 'quota_exhausted'
  | 'credential_rejected'
  | 'queue_overflow_spill'
  | 'cloud_recovered'

export interface TelemetryGapWindow {
  project_id: number
  started_at: string
  ended_at: string
  dropped_spans: number
  dropped_bytes: number
  reason: TelemetryWriteIntervalReason
  message: string
}

export interface TelemetryWriteInterval {
  mode: CloudTelemetryWriteMode
  effective_from: string
  effective_to?: string
  reason: TelemetryWriteIntervalReason
  message: string
}

export interface ProjectCloudTelemetry {
  project_id: number
  fidelity: CloudTelemetryFidelity
  attribute_allowlist: string[]
  write_mode: CloudTelemetryWriteMode
  effective_write_mode: CloudTelemetryWriteMode
  effective_reason?: TelemetryWriteIntervalReason
  effective_reason_message?: string
  cloud_write_mode_available: boolean
  reason?: string
  setup_path?: string
  queued_spans: number
  /** Spans this instance accepted for the project and gave up on delivering. */
  dead_lettered_spans: number
  /**
   * Why the most recent give-up happened. Delivery metadata only — this
   * instance's own bounded error string, never span content.
   */
  last_dead_letter_error?: string
  last_dead_letter_at?: string
  gap_windows: TelemetryGapWindow[]
  intervals: TelemetryWriteInterval[]
}

export interface CloudTelemetryWriteStatus {
  configured: boolean
  reason?: string
  setup_path?: string
  cloud_primary_projects: number
  local_mode_projects: number
  local_span_store_required: boolean
  local_span_store_reason?: string
  local_history_until?: string
  queue_depth: number
  queue_bytes: number
  queue_max_bytes: number
  oldest_unshipped_age_secs?: number
  dead_lettered_rows: number
  write_suspension?: string
  gap_windows: TelemetryGapWindow[]
  can_decommission_local_span_store: boolean
}

export interface UpdateProjectCloudTelemetryBody {
  fidelity?: CloudTelemetryFidelity
  attribute_allowlist?: string[]
  write_mode?: CloudTelemetryWriteMode
}

// ── Helpers ────────────────────────────────────────────────────────────────

/**
 * Surface the server's own sentence.
 *
 * Every gate refusal names one specific missing prerequisite — raise the
 * fidelity, link the instance, turn the telemetry switch on, re-enroll — and
 * they are four unrelated fixes. Collapsing them into "request failed" is the
 * difference between a one-minute fix and an afternoon.
 */
function throwWithDetail(error: unknown, fallback: string): never {
  const message = getErrorMessage(error)
  throw new Error(message && message !== 'Unknown error' ? message : fallback)
}

async function resolveProject(
  projectOption?: string,
): Promise<{ slug: string; id: number }> {
  const resolved = await requireProjectSlug(projectOption)
  if (resolved.source !== 'flag') {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const { data, error } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return { slug: resolved.slug, id: data.id }
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const units = ['KiB', 'MiB', 'GiB', 'TiB']
  let value = bytes / 1024
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${value.toFixed(1)} ${units[unit]}`
}

export function formatAge(seconds: number | undefined): string {
  // `undefined` is not zero. An empty queue has no oldest span, and printing
  // "0s" would read as "everything ships instantly".
  if (seconds === undefined) return '—'
  if (seconds < 60) return `${Math.round(seconds)}s`
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`
  if (seconds < 86_400) return `${(seconds / 3600).toFixed(1)}h`
  return `${(seconds / 86_400).toFixed(1)}d`
}

function describeMode(mode: CloudTelemetryWriteMode): string {
  return mode === 'cloud'
    ? 'cloud (spans are written to Temps Cloud, not stored on this instance)'
    : 'local (spans are stored on this instance)'
}

async function fetchProjectTelemetry(
  projectId: number,
): Promise<ProjectCloudTelemetry> {
  const { data, error } = await client.get<ProjectCloudTelemetry, ProblemDetails>({
    url: 'otel/cloud-telemetry/projects/{project_id}',
    path: { project_id: projectId },
  })
  if (error || !data) {
    throwWithDetail(
      error,
      `Could not read the telemetry write mode for project ${projectId}.`,
    )
  }
  return data
}

// ── Actions ────────────────────────────────────────────────────────────────

async function writeModeGet(options: {
  project?: string
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()
  const project = await resolveProject(options.project)

  const settings = await withSpinner('Reading telemetry write mode...', () =>
    fetchProjectTelemetry(project.id),
  )
  if (!settings) return

  if (options.json) {
    json(settings)
    return
  }

  newline()
  header(`${icons.globe} Telemetry storage — ${project.slug}`)
  keyValue('Write mode', describeMode(settings.write_mode))
  if (settings.effective_write_mode !== settings.write_mode) {
    keyValue('Actually writing to', describeMode(settings.effective_write_mode))
    warning(
      settings.effective_reason_message ??
        'Temps Cloud is not accepting this project’s spans right now.',
    )
  }
  keyValue('Fidelity', settings.fidelity)
  if (settings.attribute_allowlist.length > 0) {
    keyValue('Attributes allowed out', settings.attribute_allowlist.join(', '))
  }
  keyValue('Spans queued for Cloud', settings.queued_spans)

  if (settings.dead_lettered_spans > 0) {
    newline()
    warning(
      `${settings.dead_lettered_spans.toLocaleString()} span(s) were never delivered to ` +
        'Temps Cloud and will not be retried.',
    )
    if (settings.last_dead_letter_error) {
      info(`  Last failure: ${settings.last_dead_letter_error}`)
    }
    if (settings.last_dead_letter_at) {
      info(`  Most recently: ${formatDate(settings.last_dead_letter_at)}`)
    }
  }

  if (!settings.cloud_write_mode_available && settings.reason) {
    newline()
    warning('Cloud-primary writes are not available for this project yet:')
    info(`  ${settings.reason}`)
    if (settings.setup_path) {
      info(`  Set this up at: ${colors.primary(settings.setup_path)}`)
    }
  }

  if (settings.gap_windows.length > 0) {
    newline()
    warning('Spans that were never captured anywhere:')
    for (const gap of settings.gap_windows) {
      info(
        `  ${formatDate(gap.started_at)} → ${formatDate(gap.ended_at)}: ` +
          `${gap.dropped_spans.toLocaleString()} spans (${formatBytes(gap.dropped_bytes)})`,
      )
      info(`    ${gap.message}`)
    }
  }

  if (settings.intervals.length > 0) {
    newline()
    header('Storage history (newest first)')
    for (const interval of settings.intervals) {
      const until = interval.effective_to
        ? formatDate(interval.effective_to)
        : 'now'
      info(
        `  ${interval.mode === 'cloud' ? 'Temps Cloud ' : 'This instance'} ` +
          `${formatDate(interval.effective_from)} → ${until}  (${interval.reason})`,
      )
    }
  }
  newline()
}

async function writeModeSet(
  mode: string,
  options: {
    project?: string
    fidelity?: string
    force?: boolean
    yes?: boolean
    json?: boolean
  },
): Promise<void> {
  if (mode !== 'local' && mode !== 'cloud') {
    throw new Error(
      `Unknown write mode "${mode}". Use "local" (spans stored on this instance) ` +
        'or "cloud" (spans written to Temps Cloud).',
    )
  }
  if (
    options.fidelity &&
    options.fidelity !== 'metered' &&
    options.fidelity !== 'queryable'
  ) {
    throw new Error(
      `Unknown fidelity "${options.fidelity}". Use "metered" or "queryable".`,
    )
  }

  await requireAuth()
  await setupClient()
  const project = await resolveProject(options.project)

  // Switching to Cloud-primary stops this project's spans being stored on this
  // machine. That is not a setting to change by accident from a script, so it
  // confirms unless explicitly forced.
  if (mode === 'cloud' && !options.force && !options.yes) {
    const confirmed = await promptConfirm({
      message:
        `Write ${project.slug}’s spans to Temps Cloud instead of storing them here? ` +
        'New spans will not be stored on this instance at all. Spans already stored ' +
        'here stay readable until they age out of retention.',
      default: false,
    })
    if (!confirmed) {
      info('No change made.')
      return
    }
  }

  const body: UpdateProjectCloudTelemetryBody = { write_mode: mode }
  if (options.fidelity) {
    body.fidelity = options.fidelity as CloudTelemetryFidelity
  }

  const updated = await withSpinner('Updating telemetry write mode...', async () => {
    const { data, error } = await client.patch<
      ProjectCloudTelemetry,
      ProblemDetails
    >({
      url: 'otel/cloud-telemetry/projects/{project_id}',
      path: { project_id: project.id },
      body,
    })
    if (error || !data) {
      throwWithDetail(
        error,
        `Could not change the telemetry write mode for ${project.slug}.`,
      )
    }
    return data
  })
  if (!updated) return

  if (options.json) {
    json(updated)
    return
  }

  newline()
  if (updated.write_mode === 'cloud') {
    success(
      `${project.slug}’s spans now go to Temps Cloud. They are no longer stored on this instance.`,
    )
    info(
      'Traces from after this change are read back from Cloud. A query that crosses ' +
        'the switch is answered from one side and tells you where it was cut.',
    )
  } else {
    success(`${project.slug}’s spans are stored on this instance.`)
  }
  keyValue('Fidelity', updated.fidelity)
  if (updated.queued_spans > 0) {
    keyValue('Spans still queued for Cloud', updated.queued_spans)
  }
  newline()
}

async function telemetryStatus(options: { json?: boolean }): Promise<void> {
  await requireAuth()
  await setupClient()

  const status = await withSpinner(
    'Reading Cloud telemetry write status...',
    async () => {
      const { data, error } = await client.get<
        CloudTelemetryWriteStatus,
        ProblemDetails
      >({ url: 'otel/cloud-telemetry/status' })
      if (error || !data) {
        throwWithDetail(
          error,
          'Could not read this instance’s Cloud telemetry queue. Queue depth is ' +
            'unknown — not zero.',
        )
      }
      return data
    },
  )
  if (!status) return

  if (options.json) {
    json(status)
    return
  }

  newline()
  header(`${icons.globe} Cloud telemetry writes`)
  keyValue('Cloud-primary projects', status.cloud_primary_projects)
  keyValue('Projects storing spans here', status.local_mode_projects)
  keyValue('Spans queued for Cloud', status.queue_depth)
  keyValue(
    'Queue size',
    status.queue_max_bytes > 0
      ? `${formatBytes(status.queue_bytes)} of ${formatBytes(status.queue_max_bytes)}`
      : formatBytes(status.queue_bytes),
  )
  keyValue('Oldest unshipped span', formatAge(status.oldest_unshipped_age_secs))
  keyValue('Shipments that gave up', status.dead_lettered_rows)

  newline()
  if (status.local_span_store_required) {
    warning('This instance still needs its local span store.')
    if (status.local_span_store_reason) info(`  ${status.local_span_store_reason}`)
    if (status.local_history_until) {
      info(`  Local history is readable through ${formatDate(status.local_history_until)}.`)
    }
  } else {
    success('No project writes spans to this instance any more.')
    info(
      '  A local span backend is no longer required for traces. Metrics, logs and ' +
        'every other signal still use local storage — only spans move.',
    )
  }

  if (!status.configured && status.reason) {
    newline()
    warning('Cloud-primary telemetry writes are not set up:')
    info(`  ${status.reason}`)
    if (status.setup_path) {
      info(`  Set this up at: ${colors.primary(status.setup_path)}`)
    }
  }

  if (status.write_suspension) {
    newline()
    warning('Cloud-primary writes are suspended — spans are being stored here:')
    info(`  ${status.write_suspension}`)
    info('  Project settings are unchanged and resume automatically.')
  }

  if (status.gap_windows.length > 0) {
    newline()
    warning('Spans that were never captured anywhere (last 30 days):')
    for (const gap of status.gap_windows) {
      info(
        `  project ${gap.project_id}  ${formatDate(gap.started_at)} → ${formatDate(gap.ended_at)}: ` +
          `${gap.dropped_spans.toLocaleString()} spans (${formatBytes(gap.dropped_bytes)})`,
      )
      info(`    ${gap.message}`)
    }
  }
  newline()
}

// ── Registration ───────────────────────────────────────────────────────────

export function registerCloudTelemetryCommands(cloud: Command): void {
  const telemetry = cloud
    .command('telemetry')
    .description(
      'Where a project’s spans are written — this instance, or Temps Cloud (ADR-041)',
    )

  const writeMode = telemetry
    .command('write-mode')
    .description('Read or change a project’s telemetry write mode')

  writeMode
    .command('get')
    .description(
      'Show where a project’s spans are written, what is queued, and any gaps',
    )
    .option('-p, --project <slug>', 'Project slug')
    .option('--json', 'Output in JSON format')
    .action(writeModeGet)

  writeMode
    .command('set <mode>')
    .description(
      'Set the write mode to "local" (stored on this instance) or "cloud" ' +
        '(written to Temps Cloud, not stored here)',
    )
    .option('-p, --project <slug>', 'Project slug')
    .option(
      '--fidelity <tier>',
      'Also set Cloud telemetry fidelity: metered or queryable. ' +
        '"cloud" requires "queryable".',
    )
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation prompts (alias for --force)')
    .option('--json', 'Output in JSON format')
    .action(writeModeSet)

  telemetry
    .command('status')
    .description(
      'Instance-wide Cloud telemetry write status: queue depth, gaps, and whether ' +
        'the local span store is still required',
    )
    .option('--json', 'Output in JSON format')
    .action(telemetryStatus)

  // ADR-042 §10: the many-projects-at-once path. Registered on the same
  // `telemetry` command as the single-project controls above, so an operator
  // who found one has found the other.
  registerCloudTelemetryBulkCommands(telemetry)

  telemetry.addHelpText(
    'after',
    `
Cutting a project over to Temps Cloud:
  $ temps cloud telemetry write-mode get --project my-app
  $ temps cloud telemetry write-mode set cloud --project my-app --fidelity queryable
  $ temps cloud telemetry status          # is the local span store still needed?

Bringing it back:
  $ temps cloud telemetry write-mode set local --project my-app

Notes:
  • "cloud" requires queryable fidelity, a linked instance, and the Cloud
    telemetry switch on. Each refusal names the one prerequisite that is missing.
  • Setting "local" is never refused — you can always bring spans back to
    storage you control.
  • Disconnecting Temps Cloud returns every Cloud-primary project to local
    storage and writes whatever is still queued to this instance.
`,
  )
}
