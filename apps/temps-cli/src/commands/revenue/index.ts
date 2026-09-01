// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { Command } from 'commander'
import { readFile, stat } from 'node:fs/promises'
import { basename } from 'node:path'
import { requireAuth, config, credentials } from '../../config/store.js'
import {
  setupClient,
  client,
  normalizeApiUrl,
  getErrorMessage,
} from '../../lib/api-client.js'
import { getProjectBySlug } from '../../api/sdk.gen.js'
import { requireProjectSlug } from '../../config/resolve-project.js'
import {
  startSpinner,
  succeedSpinner,
  failSpinner,
  isQuietMode,
} from '../../ui/spinner.js'
import {
  colors,
  info,
  json as printJson,
  keyValue,
  newline,
  warning,
} from '../../ui/output.js'

// ---- Types (hand-written — the CLI's openapi.json predates the revenue feature) ----

interface ImportRowErrorResponse {
  row: number
  reason: string
}

interface ImportOutcomeResponse {
  rows_read: number
  inserted: number
  updated: number
  skipped_stale: number
  skipped_invalid: number
  errors: ImportRowErrorResponse[]
}

interface IntegrationResponse {
  id: number
  provider: string
  status: string
  webhook_path: string
  last_event_at: string | null
}

// ---- Options ----

type ImportKind = 'subscriptions' | 'invoices'

interface ImportOptions {
  project?: string
  integrationId?: string
  provider?: string
  json?: boolean
}

// ---- Helpers ----

async function resolveProjectId(flagValue?: string): Promise<{ id: number; slug: string }> {
  const resolved = await requireProjectSlug(flagValue)
  if (resolved.source !== 'flag' && !isQuietMode()) {
    info(`Using project ${colors.bold(resolved.slug)} (from ${resolved.source})`)
  }
  const { data, error } = await getProjectBySlug({
    client,
    path: { slug: resolved.slug },
  })
  if (error || !data) {
    throw new Error(`Project "${resolved.slug}" not found`)
  }
  return { id: data.id, slug: resolved.slug }
}

/**
 * Fetch integrations for a project via hand-written fetch. We cannot use the
 * generated SDK here because apps/temps-cli/openapi.json was last generated
 * before the revenue feature landed.
 */
async function listIntegrations(projectId: number): Promise<IntegrationResponse[]> {
  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const url = `${apiUrl}/projects/${projectId}/revenue/integrations`
  const res = await fetch(url, {
    headers: { Authorization: `Bearer ${apiKey}` },
  })
  if (!res.ok) {
    const body = await res.text()
    throw new Error(`Failed to list revenue integrations (${res.status}): ${body || res.statusText}`)
  }
  return (await res.json()) as IntegrationResponse[]
}

/**
 * Pick which integration to target out of an already-fetched list. Priority:
 *   1. --integration-id <n>
 *   2. --provider <name> (must match exactly one integration)
 *   3. If the project has exactly one integration, use it.
 */
export function selectIntegration(
  integrations: IntegrationResponse[],
  opts: Pick<ImportOptions, 'integrationId' | 'provider'>,
): IntegrationResponse {
  if (opts.integrationId) {
    const id = Number(opts.integrationId)
    if (!Number.isFinite(id) || id <= 0) {
      throw new Error(`Invalid --integration-id: "${opts.integrationId}"`)
    }
    const found = integrations.find((i) => i.id === id)
    if (!found) {
      throw new Error(`Integration ${id} not found in this project`)
    }
    return found
  }

  if (integrations.length === 0) {
    throw new Error(
      'No revenue integrations configured for this project. ' +
      'Connect a provider in the web UI first (Project → Revenue → Connect).',
    )
  }

  if (opts.provider) {
    const matches = integrations.filter((i) => i.provider === opts.provider)
    if (matches.length === 0) {
      throw new Error(
        `No integration with provider "${opts.provider}". Available: ${integrations.map((i) => i.provider).join(', ')}`,
      )
    }
    if (matches.length > 1) {
      throw new Error(
        `Multiple integrations for provider "${opts.provider}". Pass --integration-id to disambiguate.`,
      )
    }
    return matches[0]!
  }

  if (integrations.length > 1) {
    throw new Error(
      `Multiple integrations configured (${integrations.map((i) => `${i.provider} [id=${i.id}]`).join(', ')}). ` +
      'Pass --provider <name> or --integration-id <id>.',
    )
  }

  return integrations[0]!
}

/**
 * Resolve which integration to target, fetching the project's integrations
 * first. See {@link selectIntegration} for the selection priority.
 */
async function resolveIntegration(
  projectId: number,
  opts: ImportOptions,
): Promise<IntegrationResponse> {
  const integrations = await listIntegrations(projectId)
  return selectIntegration(integrations, opts)
}

async function uploadCsv(
  projectId: number,
  integrationId: number,
  kind: ImportKind,
  filePath: string,
): Promise<ImportOutcomeResponse> {
  const apiUrl = normalizeApiUrl(config.get('apiUrl'))
  const apiKey = await credentials.getApiKey()
  const url = `${apiUrl}/projects/${projectId}/revenue/integrations/${integrationId}/import/${kind}`

  const fileStat = await stat(filePath)
  if (!fileStat.isFile()) {
    throw new Error(`Not a regular file: ${filePath}`)
  }

  const bytes = await readFile(filePath)
  const arrayBuffer = new ArrayBuffer(bytes.byteLength)
  new Uint8Array(arrayBuffer).set(bytes)

  const form = new FormData()
  form.append(
    'file',
    new Blob([arrayBuffer], { type: 'text/csv' }),
    basename(filePath),
  )

  const res = await fetch(url, {
    method: 'POST',
    headers: { Authorization: `Bearer ${apiKey}` },
    body: form,
  })

  const text = await res.text()
  if (!res.ok) {
    let detail = text
    try {
      const parsed = JSON.parse(text) as { detail?: string; title?: string }
      detail = parsed.detail || parsed.title || text
    } catch {
      // keep raw text
    }
    throw new Error(`Import failed (${res.status}): ${detail || res.statusText}`)
  }

  try {
    return JSON.parse(text) as ImportOutcomeResponse
  } catch (err) {
    throw new Error(`Malformed server response: ${getErrorMessage(err)}`)
  }
}

function renderSummary(
  kind: ImportKind,
  integration: IntegrationResponse,
  outcome: ImportOutcomeResponse,
): void {
  newline()
  keyValue('Provider', integration.provider)
  keyValue('Integration', `#${integration.id}`)
  keyValue('Rows read', outcome.rows_read)
  keyValue('Inserted', outcome.inserted)
  keyValue('Updated', outcome.updated)
  keyValue('Skipped (stale)', outcome.skipped_stale)
  keyValue('Skipped (invalid)', outcome.skipped_invalid)
  keyValue('Errors', outcome.errors.length)

  if (outcome.errors.length > 0) {
    newline()
    warning(
      `First ${Math.min(outcome.errors.length, 10)} error(s) (of ${outcome.errors.length}):`,
    )
    for (const e of outcome.errors.slice(0, 10)) {
      console.log(`  ${colors.muted(`row ${e.row}:`)} ${e.reason}`)
    }
  }

  if (outcome.skipped_stale > 0) {
    newline()
    info(
      `${outcome.skipped_stale} row(s) skipped because newer webhook data already exists. ` +
      `This is expected when re-running an import over a live integration.`,
    )
  }
  if (kind === 'subscriptions' && outcome.inserted + outcome.updated > 0) {
    info('MRR metrics will reflect the imported data on the next query.')
  }
}

async function runImport(
  kind: ImportKind,
  filePath: string,
  opts: ImportOptions,
): Promise<void> {
  await requireAuth()
  await setupClient()

  const { id: projectId } = await resolveProjectId(opts.project)
  const integration = await resolveIntegration(projectId, opts)

  startSpinner(`Uploading ${kind} CSV to ${integration.provider} (#${integration.id})…`)

  let outcome: ImportOutcomeResponse
  try {
    outcome = await uploadCsv(projectId, integration.id, kind, filePath)
  } catch (err) {
    failSpinner(`${kind} import failed`)
    throw err
  }

  succeedSpinner(
    `${kind} imported: ${outcome.inserted} inserted, ${outcome.updated} updated, ` +
    `${outcome.skipped_stale + outcome.skipped_invalid} skipped, ${outcome.errors.length} error(s)`,
  )

  if (opts.json) {
    printJson(outcome)
    return
  }

  renderSummary(kind, integration, outcome)
}

// ---- Registration ----

export function registerRevenueCommands(program: Command): void {
  const revenue = program
    .command('revenue')
    .description('Manage revenue integrations and import historical data')

  const importCmd = revenue
    .command('import')
    .description('Import historical revenue data from a CSV export')

  importCmd
    .command('subscriptions <file>')
    .description('Import current subscriptions CSV (e.g., Stripe → Subscriptions → Export)')
    .option('-p, --project <slug>', 'Project slug (defaults to linked project)')
    .option('--integration-id <id>', 'Target integration ID (auto-detected if only one exists)')
    .option('--provider <name>', 'Target provider name (e.g., stripe)')
    .option('--json', 'Output the import outcome as JSON (suppresses spinners)')
    .action(async (file: string, opts: ImportOptions) => {
      await runImport('subscriptions', file, opts)
    })

  importCmd
    .command('invoices <file>')
    .description('Import paid invoices CSV to backfill the revenue chart')
    .option('-p, --project <slug>', 'Project slug (defaults to linked project)')
    .option('--integration-id <id>', 'Target integration ID (auto-detected if only one exists)')
    .option('--provider <name>', 'Target provider name (e.g., stripe)')
    .option('--json', 'Output the import outcome as JSON (suppresses spinners)')
    .action(async (file: string, opts: ImportOptions) => {
      await runImport('invoices', file, opts)
    })
}
