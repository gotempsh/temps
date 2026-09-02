// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * `temps cloud telemetry bulk-switch|bulk-status|bulk-cancel` — activate Temps
 * Cloud telemetry across many projects in one job (ADR-042 §10).
 *
 * ============================================================================
 * Hand-written request/response shapes
 * ============================================================================
 *
 * These routes live in the OTel plugin under
 * `/otel/cloud-telemetry/bulk-jobs/*` and are called through the shared
 * `client` object rather than the generated SDK — the plugin's schema is not
 * part of `openapi.json` and must never be added to it (see "Regenerating the
 * OpenAPI clients" in the root CLAUDE.md). The interfaces below mirror the
 * serde structs in
 * `crates/temps-otel/src/handlers/cloud_bulk_activation_handler.rs` and must be
 * kept in sync by hand if that shape changes.
 *
 * ============================================================================
 * Why `bulk-switch` confirms
 * ============================================================================
 *
 * Shipping history to Temps Cloud costs the operator real money, and this
 * command has no payment event behind it. So it estimates first, prints exactly
 * what would be sent per project, and only then asks. `--yes` is for scripts
 * that have already made that decision; there is deliberately no way to submit
 * without an estimate having been computed, because the plan token the server
 * requires is only ever issued by the estimate.
 */

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { client, getErrorMessage, setupClient } from '../../lib/api-client.js'
import type { ProblemDetails } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { promptConfirm } from '../../ui/prompts.js'
import { printTable, type TableColumn } from '../../ui/table.js'
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
import { formatBytes } from './telemetry.js'

// ── Shapes ─────────────────────────────────────────────────────────────────

export type BulkJobStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'completed_with_failures'
  | 'aborted'
  | 'cancelled'

export type BulkJobProjectStatus =
  | 'pending'
  | 'switching'
  | 'backfilling'
  | 'done'
  | 'failed'
  | 'skipped'

export type BulkJobTrigger = 'purchase' | 'operator'

/** `estimating` means the server has no observed throughput yet — render
 * "estimating…", never a number. */
export type BulkActivationEtaState = 'estimating' | 'known' | 'finished'

export interface EstimateBulkActivationBody {
  all_eligible_projects?: boolean
  project_ids?: number[]
  window_from?: string
  window_to?: string
}

export interface BulkActivationProjectEstimate {
  project_id: number
  eligible: boolean
  skip_reason?: string
  skip_detail?: string
  setup_path?: string
  fidelity: 'metered' | 'queryable'
  window_from: string
  window_to: string
  estimated_spans: number
  estimated_bytes: number
  sampled_spans: number
  average_span_bytes: number
}

export interface BulkActivationEstimate {
  configured: boolean
  reason?: string
  setup_path?: string
  window_from: string
  window_to: string
  projects: BulkActivationProjectEstimate[]
  total_projects: number
  eligible_projects: number
  skipped_projects: number
  estimated_spans: number
  estimated_bytes: number
  plan_token?: string
  plan_hash?: string
  plan_expires_at?: string
  active_batch_id?: string
}

export interface BulkActivationJobProject {
  project_id: number
  status: BulkJobProjectStatus
  skip_reason?: string
  skip_detail?: string
  setup_path?: string
  window_from: string
  window_to: string
  estimated_spans: number
  estimated_bytes: number
  spans_shipped: number
  bytes_shipped: number
  percent_complete?: number
  last_error?: string
  started_at?: string
  completed_at?: string
}

export interface BulkActivationJob {
  batch_id: string
  trigger: BulkJobTrigger
  status: BulkJobStatus
  requested_by_user_id?: number
  plan_hash?: string
  estimated_spans: number
  estimated_bytes: number
  spans_shipped: number
  bytes_shipped: number
  percent_complete?: number
  eta_seconds?: number
  eta_state: BulkActivationEtaState
  observed_spans_per_sec?: number
  current_project_id?: number
  projects_total: number
  projects_pending: number
  projects_done: number
  projects_failed: number
  projects_skipped: number
  cancel_requested: boolean
  cancel_requested_at?: string
  abort_reason?: string
  abort_detail?: string
  created_at: string
  started_at?: string
  completed_at?: string
  projects: BulkActivationJobProject[]
}

// ── Helpers ────────────────────────────────────────────────────────────────

/**
 * Surface the server's own sentence.
 *
 * Every refusal here names one specific thing to do — link the instance, raise
 * a project's fidelity, re-estimate an expired plan, watch the job that is
 * already running. Collapsing them into "request failed" turns a one-minute fix
 * into an afternoon for someone who has nobody to ask.
 */
function throwWithDetail(error: unknown, fallback: string): never {
  const message = getErrorMessage(error)
  throw new Error(message && message !== 'Unknown error' ? message : fallback)
}

/** Repeatable `--project <id>`. */
export function collectProjectId(value: string, previous: number[]): number[] {
  const parsed = Number.parseInt(value, 10)
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error(`Invalid --project "${value}": expected a positive project id.`)
  }
  return [...previous, parsed]
}

/** Accept an RFC 3339 timestamp, or say precisely what was wrong with it. */
export function parseTimestamp(value: string | undefined, flag: string): string | undefined {
  if (value === undefined) return undefined
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    throw new Error(
      `Invalid ${flag} "${value}": expected an RFC 3339 timestamp, e.g. 2026-08-01T00:00:00Z.`,
    )
  }
  return parsed.toISOString()
}

/**
 * A duration a human reads at a glance.
 *
 * Rendered coarsely on purpose: a false-precision countdown that jumps around
 * is worse than an honest range, and the ETA is an average over acknowledged
 * chunks rather than a schedule.
 */
export function formatEta(seconds: number | undefined, state: BulkActivationEtaState): string {
  if (state === 'finished') return '—'
  if (state === 'estimating' || seconds === undefined) return 'estimating…'
  if (seconds < 60) return 'under a minute'
  if (seconds < 3600) return `about ${Math.round(seconds / 60)} minute(s)`
  if (seconds < 86_400) return `about ${(seconds / 3600).toFixed(1)} hour(s)`
  return `about ${(seconds / 86_400).toFixed(1)} day(s)`
}

export function formatPercent(percent: number | undefined): string {
  // `undefined` is not zero. A window with nothing in it is not "0% done", and
  // showing 0% would read as stuck.
  if (percent === undefined) return '—'
  return `${percent.toFixed(1)}%`
}

/** Whether the job has stopped for good. Mirrors `BulkJobStatus::is_terminal`. */
export function isTerminal(status: BulkJobStatus): boolean {
  return status !== 'pending' && status !== 'running'
}

async function fetchEstimate(
  body: EstimateBulkActivationBody,
): Promise<BulkActivationEstimate> {
  const { data, error } = await client.post<BulkActivationEstimate, ProblemDetails>({
    url: 'otel/cloud-telemetry/bulk-jobs/estimate',
    body,
  })
  if (error || !data) {
    throwWithDetail(
      error,
      'Could not estimate the Temps Cloud activation. Nothing has been switched and nothing ' +
        'has been shipped.',
    )
  }
  return data
}

async function fetchJob(batchId: string): Promise<BulkActivationJob> {
  const { data, error } = await client.get<BulkActivationJob, ProblemDetails>({
    url: 'otel/cloud-telemetry/bulk-jobs/{batch_id}',
    path: { batch_id: batchId },
  })
  if (error || !data) {
    throwWithDetail(error, `Could not read Temps Cloud activation job ${batchId}.`)
  }
  return data
}

async function fetchCurrentJob(): Promise<BulkActivationJob | null> {
  // 200 + `null` is the ordinary answer on an instance with no activation
  // running, and is deliberately not a 404 — so an empty body here means "no
  // job", never "this server does not have this feature".
  const { data, error } = await client.get<BulkActivationJob | null, ProblemDetails>({
    url: 'otel/cloud-telemetry/bulk-jobs/current',
  })
  if (error) {
    throwWithDetail(
      error,
      'Could not read this instance’s Temps Cloud activation status.',
    )
  }
  // A successful response with no job is `null`, never an error — so an absent
  // body here means "nothing is running", not "this server cannot answer".
  return data ?? null
}

// ── Rendering ──────────────────────────────────────────────────────────────

const ESTIMATE_COLUMNS: TableColumn<BulkActivationProjectEstimate>[] = [
  { header: 'Project', accessor: (row) => row.project_id, align: 'right' },
  {
    header: 'Included',
    accessor: (row) => (row.eligible ? 'yes' : `no — ${row.skip_reason ?? 'skipped'}`),
  },
  { header: 'Fidelity', accessor: (row) => row.fidelity },
  {
    header: 'Spans',
    accessor: (row) => (row.eligible ? row.estimated_spans.toLocaleString() : '—'),
    align: 'right',
  },
  {
    header: 'Estimated metered bytes',
    accessor: (row) => (row.eligible ? formatBytes(row.estimated_bytes) : '—'),
    align: 'right',
  },
]

function printEstimate(estimate: BulkActivationEstimate): void {
  newline()
  header(`${icons.globe} Temps Cloud activation — estimate`)
  keyValue(
    'Window',
    `${formatDate(estimate.window_from)} → ${formatDate(estimate.window_to)}`,
  )
  newline()
  printTable(estimate.projects, ESTIMATE_COLUMNS)
  newline()

  keyValue('Projects to switch', estimate.eligible_projects)
  keyValue('Spans to ship', estimate.estimated_spans.toLocaleString())
  keyValue('Estimated metered bytes', formatBytes(estimate.estimated_bytes))
  info(
    '  This is a projection from a sample of each project’s spans. Temps Cloud’s own ' +
      'acknowledgement is authoritative.',
  )

  const skipped = estimate.projects.filter((project) => !project.eligible)
  if (skipped.length > 0) {
    newline()
    warning(
      `${skipped.length} project(s) will not be switched. Nothing is shipped for them, and ` +
        'this activation will not change their settings for you:',
    )
    for (const project of skipped) {
      info(`  project ${project.project_id}: ${project.skip_detail ?? project.skip_reason}`)
      if (project.setup_path) {
        info(`    Fix it at: ${colors.primary(project.setup_path)}`)
      }
    }
  }
}

function printJob(job: BulkActivationJob): void {
  newline()
  header(`${icons.globe} Temps Cloud activation ${job.batch_id}`)
  keyValue('Status', job.status)
  keyValue('Started by', job.trigger === 'purchase' ? 'a Temps Cloud purchase' : 'an operator')
  keyValue(
    'Progress',
    `${formatPercent(job.percent_complete)}  (${job.spans_shipped.toLocaleString()} of ` +
      `${job.estimated_spans.toLocaleString()} spans, ${formatBytes(job.bytes_shipped)})`,
  )
  keyValue('Time remaining', formatEta(job.eta_seconds, job.eta_state))
  keyValue(
    'Projects',
    `${job.projects_done} done · ${job.projects_skipped} skipped · ` +
      `${job.projects_failed} failed · ${job.projects_pending} to go`,
  )
  if (job.current_project_id !== undefined) {
    keyValue('Currently activating', `project ${job.current_project_id}`)
  }
  if (job.started_at) keyValue('Started', formatDate(job.started_at))
  if (job.completed_at) keyValue('Finished', formatDate(job.completed_at))

  if (job.cancel_requested && !isTerminal(job.status)) {
    newline()
    info(
      'A cancellation has been requested. The job stops at the next chunk boundary, so ' +
        'nothing already shipped is lost and nothing part-shipped is re-sent.',
    )
  }

  if (job.abort_detail) {
    newline()
    warning('This activation stopped before it finished:')
    info(`  ${job.abort_detail}`)
    info(
      '  Projects it had not reached are still pending, so resuming costs nothing you have ' +
        'already paid for.',
    )
  }

  const skipped = job.projects.filter((project) => project.status === 'skipped')
  if (skipped.length > 0) {
    newline()
    warning('Projects that were not switched:')
    for (const project of skipped) {
      info(`  project ${project.project_id}: ${project.skip_detail ?? project.skip_reason}`)
      if (project.setup_path) {
        info(`    Fix it at: ${colors.primary(project.setup_path)}`)
      }
    }
  }

  const failed = job.projects.filter((project) => project.status === 'failed')
  if (failed.length > 0) {
    newline()
    warning(
      'Projects whose history did not finish shipping. Their write mode is left on Temps ' +
        'Cloud on purpose — reverting after some spans shipped would split their history ' +
        'across both stores. Re-run this project to fill the gap; it resumes rather than ' +
        're-ships:',
    )
    for (const project of failed) {
      info(
        `  project ${project.project_id}: ${project.spans_shipped.toLocaleString()} of ` +
          `${project.estimated_spans.toLocaleString()} spans shipped`,
      )
      if (project.last_error) info(`    ${project.last_error}`)
      info(
        `    Retry with: ${colors.primary(
          `temps cloud telemetry bulk-switch --project ${project.project_id}`,
        )}`,
      )
    }
  }
  newline()
}

/**
 * Follow a job to its end, one line per poll.
 *
 * Append-only rather than a redrawn screen: an activation can run for hours,
 * often in a CI log or a detached shell, and a scrollback that shows how the
 * rate actually moved is more use afterwards than a single line that was
 * overwritten.
 */
async function watchJob(batchId: string, intervalMs = 5_000): Promise<BulkActivationJob> {
  let job = await fetchJob(batchId)
  info(
    `Watching activation ${batchId}. Press Ctrl+C to stop watching — the activation keeps ` +
      'running on the server.',
  )

  for (;;) {
    const current =
      job.current_project_id !== undefined ? ` · project ${job.current_project_id}` : ''
    info(
      `  ${job.status} · ${formatPercent(job.percent_complete)} · ` +
        `${job.spans_shipped.toLocaleString()}/${job.estimated_spans.toLocaleString()} spans · ` +
        `${formatEta(job.eta_seconds, job.eta_state)} left${current}`,
    )
    if (isTerminal(job.status)) return job
    await new Promise((resolve) => setTimeout(resolve, intervalMs))
    job = await fetchJob(batchId)
  }
}

// ── Actions ────────────────────────────────────────────────────────────────

interface BulkSwitchOptions {
  all?: boolean
  project?: number[]
  from?: string
  to?: string
  yes?: boolean
  watch?: boolean
  json?: boolean
}

async function bulkSwitch(options: BulkSwitchOptions): Promise<void> {
  const projectIds = options.project ?? []
  if (options.all && projectIds.length > 0) {
    throw new Error(
      'Pass --all or --project, not both. Being explicit about what is being switched is ' +
        'the point of the confirmation.',
    )
  }
  if (!options.all && projectIds.length === 0) {
    throw new Error(
      'Name the projects to switch with --project <id> (repeatable), or pass --all to ' +
        'switch every project still storing its spans on this instance.',
    )
  }

  await requireAuth()
  await setupClient()

  const body: EstimateBulkActivationBody = {
    window_from: parseTimestamp(options.from, '--from'),
    window_to: parseTimestamp(options.to, '--to'),
  }
  if (options.all) {
    body.all_eligible_projects = true
  } else {
    body.project_ids = projectIds
  }

  const estimate = await withSpinner(
    'Estimating what would be sent to Temps Cloud...',
    () => fetchEstimate(body),
  )
  if (!estimate) return

  if (options.json && !estimate.plan_token) {
    json(estimate)
    return
  }

  if (!estimate.configured) {
    newline()
    warning('Temps Cloud telemetry activation is not set up on this instance:')
    info(`  ${estimate.reason ?? 'Temps Cloud is not connected.'}`)
    if (estimate.setup_path) {
      info(`  Set this up at: ${colors.primary(estimate.setup_path)}`)
    }
    info(
      '  Once it is connected, this command switches every named project to Temps Cloud and ' +
        'ships its existing local history, showing progress and an ETA.',
    )
    newline()
    return
  }

  printEstimate(estimate)

  if (estimate.active_batch_id) {
    newline()
    warning(
      `An activation is already running on this instance (${estimate.active_batch_id}). Only ` +
        'one may run at a time, because this instance may have exactly one Temps Cloud ' +
        'submission in flight.',
    )
    info(
      `  Watch it with: ${colors.primary(
        `temps cloud telemetry bulk-status --watch`,
      )}`,
    )
    info(
      `  Or stop it with: ${colors.primary(
        `temps cloud telemetry bulk-cancel ${estimate.active_batch_id}`,
      )}`,
    )
    newline()
    return
  }

  if (!estimate.plan_token) {
    newline()
    warning(
      'Nothing would be switched, so there is nothing to confirm. Every project named above ' +
        'is either already on Temps Cloud or blocked by the reason listed against it.',
    )
    newline()
    return
  }

  if (!options.yes) {
    newline()
    const confirmed = await promptConfirm({
      message:
        `Switch ${estimate.eligible_projects} project(s) to Temps Cloud and ship ` +
        `${estimate.estimated_spans.toLocaleString()} span(s) ` +
        `(~${formatBytes(estimate.estimated_bytes)})? This egress is billed by Temps Cloud, ` +
        'and each project stops storing new spans on this instance from the moment it is ' +
        'switched.',
      default: false,
    })
    if (!confirmed) {
      info('No change made. Nothing was switched and nothing was shipped.')
      return
    }
  }

  const job = await withSpinner('Queueing the activation...', async () => {
    const { data, error } = await client.post<BulkActivationJob, ProblemDetails>({
      url: 'otel/cloud-telemetry/bulk-jobs',
      body: { plan_token: estimate.plan_token },
    })
    if (error || !data) {
      throwWithDetail(
        error,
        'Could not queue the Temps Cloud activation. Nothing has been switched and nothing ' +
          'has been shipped.',
      )
    }
    return data
  })
  if (!job) return

  if (options.json && !options.watch) {
    json(job)
    return
  }

  newline()
  success(`Activation ${job.batch_id} queued for ${job.projects_total} project(s).`)
  info(
    '  It runs inside the server, one project at a time, and survives a restart — no ' +
      'downtime, and no need to keep this terminal open.',
  )
  info(
    `  Check on it with: ${colors.primary('temps cloud telemetry bulk-status --watch')}`,
  )

  if (options.watch) {
    newline()
    const finished = await watchJob(job.batch_id)
    if (options.json) {
      json(finished)
      return
    }
    printJob(finished)
  }
}

async function bulkStatus(options: {
  batchId?: string
  watch?: boolean
  json?: boolean
}): Promise<void> {
  await requireAuth()
  await setupClient()

  const batchId = options.batchId
  if (batchId) {
    const job = options.watch
      ? await watchJob(batchId)
      : await withSpinner('Reading the activation...', () => fetchJob(batchId))
    if (options.json) {
      json(job)
      return
    }
    printJob(job)
    return
  }

  const current = await withSpinner(
    'Reading this instance’s Temps Cloud activation status...',
    () => fetchCurrentJob(),
  )

  if (current === null) {
    if (options.json) {
      json(null)
      return
    }
    newline()
    header(`${icons.globe} Temps Cloud activation`)
    info('No activation is running on this instance.')
    info(
      `  Start one with: ${colors.primary('temps cloud telemetry bulk-switch --all')} — it ` +
        'estimates first and asks before sending anything.',
    )
    newline()
    return
  }

  if (options.watch) {
    const finished = await watchJob(current.batch_id)
    if (options.json) {
      json(finished)
      return
    }
    printJob(finished)
    return
  }

  if (options.json) {
    json(current)
    return
  }
  printJob(current)
}

async function bulkCancel(
  batchId: string,
  options: { yes?: boolean; json?: boolean },
): Promise<void> {
  await requireAuth()
  await setupClient()

  if (!options.yes) {
    const confirmed = await promptConfirm({
      message:
        `Stop activation ${batchId}? Projects already switched stay on Temps Cloud and spans ` +
        'already shipped are already billed — cancelling stops the rest, it does not undo ' +
        'what has happened. The job can be resumed from exactly where it stopped.',
      default: false,
    })
    if (!confirmed) {
      info('No change made. The activation is still running.')
      return
    }
  }

  const job = await withSpinner('Requesting cancellation...', async () => {
    const { data, error } = await client.post<BulkActivationJob, ProblemDetails>({
      url: 'otel/cloud-telemetry/bulk-jobs/{batch_id}/cancel',
      path: { batch_id: batchId },
      body: {},
    })
    if (error || !data) {
      throwWithDetail(error, `Could not cancel Temps Cloud activation ${batchId}.`)
    }
    return data
  })
  if (!job) return

  if (options.json) {
    json(job)
    return
  }

  newline()
  if (isTerminal(job.status)) {
    success(`Activation ${batchId} had already stopped (${job.status}). Nothing to cancel.`)
  } else {
    success(`Activation ${batchId} will stop at the next chunk boundary.`)
    info(
      '  Nothing already shipped is lost, and resuming re-ships nothing you have already ' +
        'paid for.',
    )
  }
  printJob(job)
}

// ── Registration ───────────────────────────────────────────────────────────

export function registerCloudTelemetryBulkCommands(telemetry: Command): void {
  telemetry
    .command('bulk-switch')
    .description(
      'Switch many projects to Temps Cloud and ship their history in one job — estimates ' +
        'first, then asks',
    )
    .option(
      '--all',
      'Every project still storing its spans on this instance. Projects already on Temps ' +
        'Cloud are not included.',
    )
    .option(
      '-p, --project <id>',
      'A project id to switch. Repeatable. Cannot be combined with --all.',
      collectProjectId,
      [] as number[],
    )
    .option(
      '--from <timestamp>',
      'Start of the history window to ship (RFC 3339). Defaults to the oldest span local ' +
        'retention can still be holding.',
    )
    .option('--to <timestamp>', 'End of the history window to ship (RFC 3339). Defaults to now.')
    .option('-y, --yes', 'Skip the confirmation. The estimate is still computed and printed.')
    .option('--watch', 'Follow the job until it finishes')
    .option('--json', 'Output in JSON format')
    .action(bulkSwitch)

  telemetry
    .command('bulk-status [batch_id]')
    .description(
      'Show the Temps Cloud activation running on this instance — progress, ETA, skips and ' +
        'failures',
    )
    .option('--watch', 'Follow the job until it finishes')
    .option('--json', 'Output in JSON format')
    .action((batchId: string | undefined, options: { watch?: boolean; json?: boolean }) =>
      bulkStatus({ batchId, ...options }),
    )

  telemetry
    .command('bulk-cancel <batch_id>')
    .description('Stop a Temps Cloud activation at its next chunk boundary')
    .option('-y, --yes', 'Skip confirmation')
    .option('--json', 'Output in JSON format')
    .action(bulkCancel)

  telemetry.addHelpText(
    'after',
    `
Activating Temps Cloud across every project:
  $ temps cloud telemetry bulk-switch --all              # estimate, confirm, queue
  $ temps cloud telemetry bulk-status --watch            # progress and ETA
  $ temps cloud telemetry bulk-cancel <batch_id>         # stop at the next chunk

Retrying the projects that were skipped or failed:
  $ temps cloud telemetry bulk-switch --project 4 --project 9

Notes:
  • The estimate sends nothing. It counts the spans in the window and projects a
    sample of them to derive a size, so you see what it costs before it is spent.
  • One activation runs at a time, because this instance may have exactly one
    Temps Cloud submission in flight. Starting a second names the running one.
  • The job runs inside the server. There is no downtime, and it resumes from
    where it stopped after a restart without re-shipping anything.
  • A project whose Cloud telemetry fidelity is not "queryable" is skipped with
    that reason and a link. Raising fidelity costs money, so this never does it
    for you.
`,
  )
}
