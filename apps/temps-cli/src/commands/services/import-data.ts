// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `services import-data*`: copy a database from a server outside Temps into
// a database of a managed service (PostgreSQL, MariaDB/MySQL, MongoDB,
// Redis). Mirrors the console's Import data page and the
// `/external-services/{id}/data-imports` API.
//
// Not to be confused with `services import`, which adopts an existing
// Docker container as a managed service.

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, getErrorMessage } from '../../lib/api-client.js'
import {
  cancelDataImport,
  getDataImport,
  getDataImportAvailability,
  listDataImports,
  startDataImport,
} from '../../api/sdk.gen.js'
import type {
  DataImportAvailabilityResponse,
  DataImportRunListResponse,
  DataImportRunResponse,
} from '../../api/types.gen.js'
import {
  failSpinner,
  startSpinner,
  succeedSpinner,
  updateSpinner,
  withSpinner,
} from '../../ui/spinner.js'
import { printTable, type TableColumn } from '../../ui/table.js'
import { promptConfirm, promptPassword, promptText } from '../../ui/prompts.js'
import {
  colors,
  error as errorOutput,
  formatRelativeTime,
  header,
  icons,
  info,
  json as jsonOut,
  keyValue,
  newline,
  success,
  warning,
} from '../../ui/output.js'
import { formatBytes } from './restore.js'

// ---- Option types ----------------------------------------------------------

interface ServiceOptions {
  id: string
  json?: boolean
}

interface ImportDataOptions {
  id: string
  target: string
  sourceUrl?: string
  sourceUrlEnv?: string
  replace?: boolean
  confirmTarget?: string
  timeout?: string
  yes?: boolean
  wait?: boolean
  json?: boolean
}

interface RunsOptions {
  id: string
  page?: string
  pageSize?: string
  json?: boolean
}

interface RunOptions {
  id: string
  run: string
  json?: boolean
}

// ---- Pure helpers (unit-tested) ---------------------------------------------

/** Where the source connection string comes from, in order of preference. */
export type SourceUrlChoice =
  { kind: 'env'; name: string } | { kind: 'flag' } | { kind: 'prompt' } | { kind: 'missing' }

/**
 * Pick how to read the source connection string. An environment variable is
 * preferred: a value on the command line lands in shell history and is
 * visible to every user of the machine in the process list.
 */
export function chooseSourceUrl(
  options: Pick<ImportDataOptions, 'sourceUrl' | 'sourceUrlEnv'>,
  interactive: boolean,
): SourceUrlChoice {
  if (options.sourceUrlEnv) return { kind: 'env', name: options.sourceUrlEnv }
  if (options.sourceUrl) return { kind: 'flag' }
  return interactive ? { kind: 'prompt' } : { kind: 'missing' }
}

/** The connection string with user and password replaced by `***`. */
export function maskConnectionString(url: string): string {
  return url.replace(/^([a-z][a-z0-9+.-]*:\/\/)[^@/?#]*@/i, '$1***:***@')
}

/**
 * Check `--replace` / `--confirm-target` before anything is sent. Returns an
 * error message, or null when the combination is valid. The server enforces
 * the same rule; checking here gives the reason without a round-trip.
 */
export function replaceConfirmationProblem(
  options: Pick<ImportDataOptions, 'replace' | 'confirmTarget' | 'target'>,
): string | null {
  if (!options.replace) {
    return options.confirmTarget ? '--confirm-target only applies together with --replace.' : null
  }
  if (options.confirmTarget === undefined) return null // asked interactively
  return options.confirmTarget === options.target
    ? null
    : `--confirm-target must repeat the target database name exactly ('${options.target}').`
}

/** Validate `--timeout` (minutes) against the server's published bounds. */
export function parseTimeoutMinutes(
  value: string | undefined,
  max: number,
): number | undefined | string {
  if (value === undefined) return undefined
  const minutes = Number(value)
  if (!Number.isInteger(minutes) || minutes < 1 || minutes > max) {
    return `--timeout must be a whole number of minutes between 1 and ${max}.`
  }
  return minutes
}

export function isRunActive(run: Pick<DataImportRunResponse, 'status'>): boolean {
  return run.status === 'running'
}

/** One line describing how a run ended, for terminal output. */
export function describeOutcome(
  run: Pick<
    DataImportRunResponse,
    'status' | 'target_object_count' | 'target_size_bytes' | 'error_message'
  >,
  objectNoun = 'object',
): string {
  switch (run.status) {
    case 'succeeded': {
      const parts: string[] = []
      if (typeof run.target_object_count === 'number') {
        const n = run.target_object_count
        parts.push(`${n} ${n === 1 ? objectNoun : `${objectNoun}s`}`)
      }
      if (typeof run.target_size_bytes === 'number') {
        parts.push(formatBytes(run.target_size_bytes))
      }
      return parts.length ? `Imported ${parts.join(', ')}` : 'Imported'
    }
    case 'running':
      return 'Still running'
    default:
      return `${run.status[0]?.toUpperCase() ?? ''}${run.status.slice(1)}: ${
        run.error_message ?? 'no reason recorded'
      }`
  }
}

// ---- Shared ---------------------------------------------------------------

function parseId(value: string, what: string): number {
  const id = Number(value)
  if (!Number.isSafeInteger(id) || id <= 0) {
    errorOutput(`Invalid ${what}: ${value}`)
    process.exit(1)
  }
  return id
}

async function fetchAvailability(serviceId: number): Promise<DataImportAvailabilityResponse> {
  const { data, error } = await getDataImportAvailability({
    path: { id: serviceId },
  })
  if (error) throw new Error(getErrorMessage(error))
  return data as DataImportAvailabilityResponse
}

/** The run's own status word, coloured by outcome. */
export function colorStatus(status: string): string {
  switch (status) {
    case 'succeeded':
      return colors.success(status)
    case 'running':
      return colors.info(status)
    case 'cancelled':
      return colors.muted(status)
    case 'interrupted':
      return colors.warning(status)
    default:
      return colors.error(status)
  }
}

function printRun(run: DataImportRunResponse, objectNoun: string): void {
  newline()
  header(`${icons.info} Data import ${run.id}`)
  keyValue('Status', colorStatus(run.status))
  keyValue('Phase', run.phase)
  keyValue('Outcome', describeOutcome(run, objectNoun))
  keyValue('Source', run.source)
  keyValue('Source database', run.source_database)
  keyValue('Target database', run.target_database)
  keyValue('Replaced existing data', run.replace_existing ? 'yes' : 'no')
  keyValue('All or nothing', run.atomic ? 'yes' : 'no — a failure can leave partial data')
  keyValue('Started', `${run.started_at} (${formatRelativeTime(run.started_at)})`)
  if (run.finished_at) keyValue('Finished', run.finished_at)
  if (run.started_by) {
    keyValue('Started by', `${run.started_by.name} <${run.started_by.email}>`)
  }
  if (run.helper_output) {
    newline()
    info('Transfer output:')
    console.log(colors.muted(run.helper_output))
  }
  newline()
}

async function pollRun(
  serviceId: number,
  runId: number,
  timeoutSeconds: number,
): Promise<DataImportRunResponse> {
  startSpinner('Waiting for the import to finish...')
  // The server stops the transfer at its own time limit; allow a margin for
  // preparing and measuring the target on either side of it.
  const deadline = Date.now() + (timeoutSeconds + 300) * 1000
  let lastPhase = ''
  let failures = 0
  while (Date.now() < deadline) {
    const { data, error } = await getDataImport({
      path: { id: serviceId, run_id: runId },
    })
    if (error) {
      failures += 1
      if (failures > 5) {
        failSpinner(`Could not read the import's status: ${getErrorMessage(error)}`)
        throw new Error(getErrorMessage(error))
      }
    } else if (data) {
      failures = 0
      const run = data as DataImportRunResponse
      if (run.phase !== lastPhase) {
        lastPhase = run.phase
        updateSpinner(`Phase: ${run.phase.replace(/_/g, ' ')}`)
      }
      if (!isRunActive(run)) {
        if (run.status === 'succeeded') succeedSpinner('Import finished')
        else failSpinner(`Import ${run.status}`)
        return run
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 2000))
  }
  failSpinner('Stopped waiting; the import may still be running.')
  throw new Error(
    `Gave up waiting for import ${runId}. Check it with: services import-data-run --id ${serviceId} --run ${runId}`,
  )
}

// ---- Actions ----------------------------------------------------------------

async function availabilityAction(options: ServiceOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const serviceId = parseId(options.id, 'service id')
  const availability = await withSpinner('Checking import support...', () =>
    fetchAvailability(serviceId),
  )
  if (options.json) {
    jsonOut(availability)
    return
  }
  newline()
  header(`${icons.info} Data import for service ${serviceId} (${availability.service_type})`)
  keyValue('Supported', availability.supported ? colors.success('yes') : colors.muted('no'))
  keyValue('Available now', availability.available ? colors.success('yes') : colors.muted('no'))
  if (availability.reason) keyValue('Reason', availability.reason)
  const spec = availability.spec
  if (spec) {
    keyValue('Engine', spec.engine_label)
    keyValue('Source schemes', spec.source_schemes.map((s) => `${s}://`).join(', '))
    keyValue('Example', spec.source_url_example)
    if (spec.allowed_source_options.length) {
      keyValue('Source options', spec.allowed_source_options.join(', '))
    }
    keyValue('All or nothing', spec.atomic ? 'yes' : 'no — a failure can leave partial data')
    keyValue('Target name limit', `${spec.max_target_length} characters`)
  }
  keyValue(
    'Time limit',
    `${availability.default_timeout_minutes} min by default, up to ${availability.max_timeout_minutes}`,
  )
  newline()
}

async function importDataAction(options: ImportDataOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const serviceId = parseId(options.id, 'service id')

  const confirmProblem = replaceConfirmationProblem(options)
  if (confirmProblem) {
    errorOutput(confirmProblem)
    process.exit(1)
  }

  const availability = await withSpinner('Checking import support...', () =>
    fetchAvailability(serviceId),
  )
  if (!availability.supported || !availability.available || !availability.spec) {
    errorOutput(
      `Service ${serviceId} (${availability.service_type}) cannot receive imported data now: ${
        availability.reason ?? 'unknown reason'
      }`,
    )
    process.exit(1)
  }
  const spec = availability.spec

  const timeout = parseTimeoutMinutes(options.timeout, availability.max_timeout_minutes)
  if (typeof timeout === 'string') {
    errorOutput(timeout)
    process.exit(1)
  }
  if (options.target.length > spec.max_target_length) {
    errorOutput(
      `Target '${options.target}' is longer than the ${spec.max_target_length} characters ${spec.engine_label} allows.`,
    )
    process.exit(1)
  }

  const interactive = Boolean(process.stdin.isTTY) && !options.yes
  const choice = chooseSourceUrl(options, Boolean(process.stdin.isTTY))
  let sourceUrl: string
  switch (choice.kind) {
    case 'env': {
      const value = process.env[choice.name]
      if (!value) {
        errorOutput(`Environment variable ${choice.name} is not set or empty.`)
        process.exit(1)
      }
      sourceUrl = value
      break
    }
    case 'flag':
      sourceUrl = options.sourceUrl as string
      if (!options.json) {
        warning(
          'The connection string was passed on the command line, where shell history and the process list can expose it. Prefer --source-url-env.',
        )
      }
      break
    case 'prompt':
      sourceUrl = await promptPassword({
        message: `Source connection string (e.g. ${spec.source_url_example})`,
        validate: (v: string) => (v.trim() ? true : 'Required'),
      })
      break
    case 'missing':
      errorOutput(
        'No source connection string: pass --source-url-env <VAR> (recommended) or --source-url.',
      )
      process.exit(1)
  }
  sourceUrl = sourceUrl.trim()

  let confirmTarget = options.confirmTarget
  if (options.replace && confirmTarget === undefined) {
    if (!interactive) {
      errorOutput(
        '--replace drops the target database first: pass --confirm-target <name> repeating its name.',
      )
      process.exit(1)
    }
    confirmTarget = await promptText({
      message: `Type '${options.target}' to confirm dropping it and importing into a fresh database`,
    })
    if (confirmTarget !== options.target) {
      warning('The name did not match. Aborted.')
      return
    }
  }

  if (interactive) {
    newline()
    header(`${icons.arrow} Import into '${options.target}' of service ${serviceId}`)
    keyValue('Source', maskConnectionString(sourceUrl))
    keyValue('Target database', options.target)
    keyValue(
      'Existing data',
      options.replace ? colors.error('DROPPED and replaced') : 'must be empty (refused otherwise)',
    )
    keyValue('All or nothing', spec.atomic ? 'yes' : 'no — a failure can leave partial data')
    newline()
    const go = await promptConfirm({
      message: 'Start the import?',
      default: false,
    })
    if (!go) {
      warning('Aborted.')
      return
    }
  }

  const run = await withSpinner('Starting import...', async () => {
    const { data, error } = await startDataImport({
      path: { id: serviceId },
      body: {
        source_url: sourceUrl,
        target_database: options.target,
        replace: options.replace === true,
        confirm_target_database: options.replace ? confirmTarget : null,
        timeout_minutes: timeout ?? null,
      },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as DataImportRunResponse
  })

  if (options.wait === false) {
    if (options.json) jsonOut(run)
    else {
      success(`Import ${run.id} started into '${run.target_database}'.`)
      info(`Follow it with: services import-data-run --id ${serviceId} --run ${run.id}`)
    }
    return
  }
  if (!options.json) success(`Import ${run.id} started into '${run.target_database}'.`)

  const finished = await pollRun(serviceId, run.id, run.timeout_seconds)
  if (options.json) {
    jsonOut(finished)
  } else if (finished.status === 'succeeded') {
    success(describeOutcome(finished, spec.object_noun))
  } else {
    errorOutput(describeOutcome(finished, spec.object_noun))
    if (finished.helper_output) {
      info('Transfer output:')
      console.log(colors.muted(finished.helper_output))
    }
  }
  if (finished.status !== 'succeeded') process.exitCode = 1
}

async function listRunsAction(options: RunsOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const serviceId = parseId(options.id, 'service id')
  const page = options.page ? parseId(options.page, 'page') : 1
  const pageSize = options.pageSize ? parseId(options.pageSize, 'page size') : 20

  const list = await withSpinner('Fetching data imports...', async () => {
    const { data, error } = await listDataImports({
      path: { id: serviceId },
      query: { page, page_size: pageSize },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as DataImportRunListResponse
  })

  if (options.json) {
    jsonOut(list)
    return
  }
  if (list.items.length === 0) {
    info('No data imports for this service yet.')
    return
  }
  const columns: TableColumn<DataImportRunResponse>[] = [
    { header: 'ID', accessor: (r) => String(r.id) },
    { header: 'Target', accessor: (r) => r.target_database },
    { header: 'Source', accessor: (r) => r.source },
    {
      header: 'Status',
      accessor: (r) => r.status,
      color: (_v, r) => colorStatus(r.status),
    },
    { header: 'Phase', accessor: (r) => r.phase },
    { header: 'Started', accessor: (r) => formatRelativeTime(r.started_at) },
  ]
  newline()
  header(`${icons.info} Data imports for service ${serviceId}`)
  printTable(list.items, columns)
  info(`Page ${list.page} · ${list.items.length} of ${list.total}`)
  newline()
}

async function showRunAction(options: RunOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const serviceId = parseId(options.id, 'service id')
  const runId = parseId(options.run, 'run id')
  const run = await withSpinner('Fetching data import...', async () => {
    const { data, error } = await getDataImport({
      path: { id: serviceId, run_id: runId },
    })
    if (error) throw new Error(getErrorMessage(error))
    return data as DataImportRunResponse
  })
  if (options.json) {
    jsonOut(run)
    return
  }
  printRun(
    run,
    run.service_type === 'mongodb' ? 'collection' : run.service_type === 'redis' ? 'key' : 'table',
  )
}

async function cancelRunAction(options: RunOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const serviceId = parseId(options.id, 'service id')
  const runId = parseId(options.run, 'run id')
  const { data, error } = await cancelDataImport({
    path: { id: serviceId, run_id: runId },
  })
  if (error) {
    errorOutput(`Could not cancel data import ${runId}: ${getErrorMessage(error)}`)
    process.exit(1)
  }
  if (options.json) {
    jsonOut(data)
    return
  }
  success(`Cancellation requested for data import ${runId}; it stops shortly.`)
}

// ---- Registration -----------------------------------------------------------

export function registerImportDataCommands(services: Command): void {
  services
    .command('import-data-availability')
    .description(
      'Show whether a service can receive data imported from an external database, and what source it accepts',
    )
    .requiredOption('--id <id>', 'Service ID')
    .option('--json', 'Output in JSON format')
    .action(availabilityAction)

  services
    .command('import-data')
    .description(
      'Copy a database from an external server into a database of this service (PostgreSQL, MariaDB/MySQL, MongoDB, Redis)',
    )
    .requiredOption('--id <id>', 'Service ID to import into')
    .requiredOption(
      '--target <name>',
      'Database that receives the data (created if missing; for Redis, the resource name, usually <project>_<environment>)',
    )
    .option(
      '--source-url-env <var>',
      'Name of an environment variable holding the source connection string (recommended: keeps the password out of shell history)',
    )
    .option(
      '--source-url <url>',
      'Source connection string (visible in shell history and the process list; prefer --source-url-env)',
    )
    .option('--replace', 'Drop the target database first if it already holds data (DESTRUCTIVE)')
    .option(
      '--confirm-target <name>',
      'Required with --replace when not interactive: repeat the target name',
    )
    .option(
      '--timeout <minutes>',
      'Stop the copy after this many minutes (server default when omitted)',
    )
    .option('-y, --yes', 'Skip the confirmation prompt')
    .option('--no-wait', 'Return after starting instead of waiting for the result')
    .option('--json', 'Output in JSON format')
    .action(importDataAction)

  services
    .command('import-data-runs')
    .description('List data imports into a service, newest first')
    .requiredOption('--id <id>', 'Service ID')
    .option('--page <n>', 'Page number (default 1)')
    .option('--page-size <n>', 'Items per page (default 20, max 100)')
    .option('--json', 'Output in JSON format')
    .action(listRunsAction)

  services
    .command('import-data-run')
    .description(
      'Show one data import: outcome, phase, timings, who started it and the transfer output',
    )
    .requiredOption('--id <id>', 'Service ID')
    .requiredOption('--run <id>', 'Data import ID')
    .option('--json', 'Output in JSON format')
    .action(showRunAction)

  services
    .command('import-data-cancel')
    .description('Cancel a running data import (refused once the data has been copied)')
    .requiredOption('--id <id>', 'Service ID')
    .requiredOption('--run <id>', 'Data import ID')
    .option('--json', 'Output in JSON format')
    .action(cancelRunAction)
}
