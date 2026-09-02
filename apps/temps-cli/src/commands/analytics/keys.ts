// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  listAnalyticsIngestKeys,
  createAnalyticsIngestKey,
  updateAnalyticsIngestKey,
  rotateAnalyticsIngestKey,
  revokeAnalyticsIngestKey,
  getProjectBySlug,
} from '../../api/sdk.gen.js'
import type {
  AnalyticsIngestKey,
  CreateAnalyticsIngestKeyRequest,
  UpdateAnalyticsIngestKeyRequest,
} from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable, statusBadge, type TableColumn } from '../../ui/table.js'
import { promptConfirm, promptText } from '../../ui/prompts.js'
import {
  newline,
  header,
  icons,
  json,
  colors,
  success,
  info,
  warning,
  keyValue,
} from '../../ui/output.js'

interface ListOptions {
  project?: string
  json?: boolean
}

interface CreateOptions {
  project?: string
  name?: string
  environmentId?: string
  allowedOrigins?: string[]
  rateLimit?: string
  yes?: boolean
  json?: boolean
}

interface UpdateOptions {
  project?: string
  keyId: string
  name?: string
  allowedOrigins?: string[]
  clearOrigins?: boolean
  rateLimit?: string
  clearRateLimit?: boolean
  json?: boolean
}

interface RotateOptions {
  project?: string
  keyId: string
  force?: boolean
  yes?: boolean
  json?: boolean
}

interface RevokeOptions {
  project?: string
  keyId: string
  force?: boolean
  yes?: boolean
}

/** Resolve project slug/ID flag → numeric project ID, per the CLI-wide `-p, --project` convention. */
async function resolveProjectId(flagValue?: string): Promise<number> {
  const resolved = await requireProjectSlug(flagValue)
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
  return data.id
}

export function registerAnalyticsKeysCommands(analytics: Command): void {
  const keys = analytics
    .command('keys')
    .description(
      'Manage analytics ingest keys (pa_...) for apps Temps does not deploy',
    )

  keys
    .command('list')
    .alias('ls')
    .description('List analytics ingest keys for a project')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('--json', 'Output in JSON format')
    .action(listIngestKeysAction)

  keys
    .command('create')
    .alias('add')
    .description('Mint a new analytics ingest key')
    .option('-p, --project <project>', 'Project slug or ID')
    .option('-n, --name <name>', 'Operator-facing label for the key')
    .option(
      '--environment-id <id>',
      'Scope the key to one environment (omit for a project-wide key)',
    )
    .option(
      '--allowed-origins <origins...>',
      'Browser origins allowed to use this key (omit to allow any origin)',
    )
    .option(
      '--rate-limit <n>',
      'Requests per minute (omit for the server default; 0 or less for unlimited)',
    )
    .option('-y, --yes', 'Skip confirmation prompts (for automation)')
    .option('--json', 'Output in JSON format')
    .action(createIngestKeyAction)

  keys
    .command('update')
    .description("Update an ingest key's label, origin allowlist, or rate limit")
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--key-id <id>', 'Analytics ingest key ID')
    .option('-n, --name <name>', 'New operator-facing label')
    .option(
      '--allowed-origins <origins...>',
      'Replace the origin allowlist with these origins',
    )
    .option('--clear-origins', 'Clear the origin allowlist (allow any origin)')
    .option('--rate-limit <n>', 'New requests-per-minute limit')
    .option('--clear-rate-limit', 'Clear the rate limit (unlimited)')
    .option('--json', 'Output in JSON format')
    .action(updateIngestKeyAction)

  keys
    .command('rotate')
    .description('Replace an ingest key value, keeping the same row and scope')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--key-id <id>', 'Analytics ingest key ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .option('--json', 'Output in JSON format')
    .action(rotateIngestKeyAction)

  keys
    .command('revoke')
    .description('Revoke (deactivate) an analytics ingest key')
    .option('-p, --project <project>', 'Project slug or ID')
    .requiredOption('--key-id <id>', 'Analytics ingest key ID')
    .option('-f, --force', 'Skip confirmation')
    .option('-y, --yes', 'Skip confirmation (alias for --force)')
    .action(revokeIngestKeyAction)

  keys.addHelpText(
    'after',
    `
Analytics ingest keys let an app that Temps does NOT deploy send analytics,
session replay and performance events. The key value (pa_...) is NOT a secret:
it is designed to ship in client-side JavaScript, so it is always shown in full
and there is no "reveal" step.

Examples:
  $ temps analytics keys list -p my-site
  $ temps analytics keys create -p my-site --name "marketing site" -y
  $ temps analytics keys create -p my-site --environment-id 12 --allowed-origins https://example.com https://www.example.com
  $ temps analytics keys update -p my-site --key-id 3 --rate-limit 1200
  $ temps analytics keys update -p my-site --key-id 3 --clear-origins
  $ temps analytics keys rotate -p my-site --key-id 3 --force
  $ temps analytics keys revoke -p my-site --key-id 3 --force`,
  )
}

async function listIngestKeysAction(options: ListOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await resolveProjectId(options.project)

  const keys = await withSpinner('Fetching analytics ingest keys...', async () => {
    const { data, error } = await listAnalyticsIngestKeys({
      client,
      path: { project_id: projectId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data ?? []
  })

  if (options.json) {
    json(keys)
    return
  }

  newline()
  header(`${icons.key} Analytics ingest keys (${keys.length})`)

  if (keys.length === 0) {
    info('No analytics ingest keys found for this project')
    info(
      'You only need one when Temps does not deploy the app sending the events.',
    )
    info(`Run: temps analytics keys create -p ${projectId} --name my-site -y`)
    newline()
    return
  }

  const columns: TableColumn<AnalyticsIngestKey>[] = [
    { header: 'ID', key: 'id', width: 6 },
    { header: 'Name', key: 'name', color: (v) => colors.bold(v) },
    {
      header: 'Status',
      accessor: (k) => (k.is_active ? 'active' : 'revoked'),
      color: (v) => statusBadge(v === 'active' ? 'active' : 'inactive'),
    },
    { header: 'Environment', accessor: (k) => formatScope(k.environment_id) },
    // Shown in full on purpose: this value is not a secret, and a truncated
    // key cannot be copied into an app.
    { header: 'Public Key', accessor: (k) => k.public_key },
    {
      header: 'Rate Limit',
      accessor: (k) => formatRateLimit(k.rate_limit_per_minute),
    },
    { header: 'Events', accessor: (k) => String(k.event_count) },
    {
      header: 'Created',
      accessor: (k) => new Date(k.created_at).toLocaleDateString(),
    },
  ]

  printTable(keys, columns, { style: 'minimal' })
  newline()
}

async function createIngestKeyAction(options: CreateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await resolveProjectId(options.project)

  let environmentId: number | undefined
  if (options.environmentId !== undefined) {
    environmentId = parseInt(options.environmentId, 10)
    if (isNaN(environmentId)) {
      warning('Invalid environment ID')
      return
    }
  }

  let rateLimit: number | undefined
  if (options.rateLimit !== undefined) {
    rateLimit = parseInt(options.rateLimit, 10)
    if (isNaN(rateLimit)) {
      warning('Invalid --rate-limit: expected an integer')
      return
    }
  }

  let origins: string[] | undefined
  if (options.allowedOrigins !== undefined) {
    origins = parseOrigins(options.allowedOrigins)
    if (origins.length === 0) {
      warning('--allowed-origins was empty; omit it to allow any origin')
      return
    }
  }

  let name: string | undefined = options.name
  if (!options.yes && !options.json && !name) {
    name = await promptText({
      message: 'Ingest key name',
      default: 'Default ingest key',
      required: true,
    })
  }

  // Build the body by conditionally including keys so an unset flag is a
  // genuinely absent field and the server applies its own default.
  const body: CreateAnalyticsIngestKeyRequest = {}
  if (name !== undefined) body.name = name
  if (environmentId !== undefined) body.environment_id = environmentId
  if (origins !== undefined) body.allowed_origins = origins
  if (rateLimit !== undefined) body.rate_limit_per_minute = rateLimit

  const result = await withSpinner('Creating analytics ingest key...', async () => {
    const { data, error } = await createAnalyticsIngestKey({
      client,
      path: { project_id: projectId },
      body,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!result) return

  if (options.json) {
    json(result)
    return
  }

  newline()
  success('Analytics ingest key created')
  newline()
  displayIngestKeyDetails(result)
  info('This value is not a secret — it is safe to embed in client-side JavaScript.')
}

async function updateIngestKeyAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await resolveProjectId(options.project)

  const keyId = parseInt(options.keyId, 10)
  if (isNaN(keyId)) {
    warning('Invalid ingest key ID')
    return
  }

  const built = buildIngestKeyUpdateBody(options)
  if (!built.ok) {
    warning(built.error)
    return
  }

  const result = await withSpinner('Updating analytics ingest key...', async () => {
    const { data, error } = await updateAnalyticsIngestKey({
      client,
      path: { project_id: projectId, key_id: keyId },
      body: built.body,
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!result) return

  if (options.json) {
    json(result)
    return
  }

  newline()
  success('Analytics ingest key updated')
  newline()
  displayIngestKeyDetails(result)
}

async function rotateIngestKeyAction(options: RotateOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await resolveProjectId(options.project)

  const keyId = parseInt(options.keyId, 10)
  if (isNaN(keyId)) {
    warning('Invalid ingest key ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning('Rotating replaces the key value immediately.')
    warning('Any app still sending the old value will start receiving 401s.')
    const confirmed = await promptConfirm({
      message: `Rotate analytics ingest key ${keyId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  const result = await withSpinner('Rotating analytics ingest key...', async () => {
    const { data, error } = await rotateAnalyticsIngestKey({
      client,
      path: { project_id: projectId, key_id: keyId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
    return data
  })

  if (!result) return

  if (options.json) {
    json(result)
    return
  }

  newline()
  success('Analytics ingest key rotated')
  newline()
  displayIngestKeyDetails(result)
  warning('Roll the new key out to your app — the previous value no longer works.')
}

async function revokeIngestKeyAction(options: RevokeOptions): Promise<void> {
  await requireAuth()
  await setupClient()

  const projectId = await resolveProjectId(options.project)

  const keyId = parseInt(options.keyId, 10)
  if (isNaN(keyId)) {
    warning('Invalid ingest key ID')
    return
  }

  const skipConfirmation = options.force || options.yes

  if (!skipConfirmation) {
    warning('This will revoke the ingest key immediately.')
    warning(
      'Analytics, session replay and performance events sent with it stop being recorded.',
    )
    const confirmed = await promptConfirm({
      message: `Revoke analytics ingest key ${keyId}?`,
      default: false,
    })
    if (!confirmed) {
      info('Cancelled')
      return
    }
  }

  await withSpinner('Revoking analytics ingest key...', async () => {
    const { error } = await revokeAnalyticsIngestKey({
      client,
      path: { project_id: projectId, key_id: keyId },
    })
    if (error) {
      throw new Error(getErrorMessage(error))
    }
  })

  success('Analytics ingest key revoked')
  info('The key is now inactive; the row is kept so past events stay attributed.')
}

export interface UpdateIngestKeyFlags {
  name?: string
  allowedOrigins?: string[]
  clearOrigins?: boolean
  rateLimit?: string
  clearRateLimit?: boolean
}

export type UpdateIngestKeyBodyResult =
  | { ok: true; body: UpdateAnalyticsIngestKeyRequest }
  | { ok: false; error: string }

/**
 * Builds the PATCH body under the endpoint's three-state semantics: a field is
 * only present when its flag (or its `--clear-*` counterpart) was actually
 * passed, so an omitted flag leaves the stored value untouched rather than
 * clearing it.
 */
export function buildIngestKeyUpdateBody(
  flags: UpdateIngestKeyFlags,
): UpdateIngestKeyBodyResult {
  if (flags.allowedOrigins !== undefined && flags.clearOrigins) {
    return {
      ok: false,
      error: 'Pass either --allowed-origins or --clear-origins, not both',
    }
  }
  if (flags.rateLimit !== undefined && flags.clearRateLimit) {
    return {
      ok: false,
      error: 'Pass either --rate-limit or --clear-rate-limit, not both',
    }
  }

  const body: UpdateAnalyticsIngestKeyRequest = {}

  if (flags.name !== undefined) {
    if (flags.name.trim().length === 0) {
      return { ok: false, error: '--name cannot be empty' }
    }
    body.name = flags.name
  }

  if (flags.clearOrigins) {
    body.allowed_origins = null
  } else if (flags.allowedOrigins !== undefined) {
    const origins = parseOrigins(flags.allowedOrigins)
    if (origins.length === 0) {
      return {
        ok: false,
        error: '--allowed-origins was empty; use --clear-origins to allow any origin',
      }
    }
    body.allowed_origins = origins
  }

  if (flags.clearRateLimit) {
    body.rate_limit_per_minute = null
  } else if (flags.rateLimit !== undefined) {
    const parsed = parseInt(flags.rateLimit, 10)
    if (isNaN(parsed)) {
      return { ok: false, error: 'Invalid --rate-limit: expected an integer' }
    }
    body.rate_limit_per_minute = parsed
  }

  if (Object.keys(body).length === 0) {
    return {
      ok: false,
      error:
        'Nothing to update. Pass --name, --allowed-origins, --clear-origins, --rate-limit or --clear-rate-limit',
    }
  }

  return { ok: true, body }
}

/** Accepts both repeated values and comma-separated lists, trimming blanks. */
export function parseOrigins(values: string[]): string[] {
  return values
    .flatMap((value) => value.split(','))
    .map((value) => value.trim())
    .filter((value) => value.length > 0)
}

/** `null`/`[]` means the server accepts the key from any origin. */
export function formatOrigins(origins: string[] | null | undefined): string {
  if (!origins || origins.length === 0) return 'any origin'
  return origins.join(', ')
}

/** `null` or a non-positive value means the key is not rate limited. */
export function formatRateLimit(limit: number | null | undefined): string {
  if (limit === null || limit === undefined || limit <= 0) return 'unlimited'
  return `${limit}/min`
}

/** A key without an environment is scoped to the whole project. */
export function formatScope(environmentId: number | null | undefined): string {
  if (environmentId === null || environmentId === undefined) return 'project-wide'
  return String(environmentId)
}

function displayIngestKeyDetails(key: AnalyticsIngestKey): void {
  header(`${icons.key} ${key.name}`)
  keyValue('ID', key.id)
  // Never masked: the key is meant to be public, and hiding it would only make
  // the operator hunt for a reveal step that does not exist.
  keyValue('Public Key', colors.bold(key.public_key))
  keyValue(
    'Status',
    key.is_active ? colors.success('Active') : colors.muted('Revoked'),
  )
  keyValue('Environment', formatScope(key.environment_id))
  keyValue('Allowed Origins', formatOrigins(key.allowed_origins))
  keyValue('Rate Limit', formatRateLimit(key.rate_limit_per_minute))
  keyValue('Events', key.event_count)
  keyValue(
    'Last Used',
    key.last_used_at ? new Date(key.last_used_at).toLocaleString() : 'never',
  )
  keyValue('Created', new Date(key.created_at).toLocaleString())
  if (key.revoked_at) {
    keyValue('Revoked', new Date(key.revoked_at).toLocaleString())
  }
  newline()
}
