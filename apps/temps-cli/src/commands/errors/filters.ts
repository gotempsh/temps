// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps errors filters`: server-side inbound filters that drop noisy error
// events before they are stored (ADR 111, D1.4).

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createErrorInboundFilter,
  deleteErrorInboundFilter,
  getErrorInboundFilter,
  listErrorInboundFilters,
  updateErrorInboundFilter,
} from '../../api/sdk.gen.js'
import { endpoint } from '../../lib/endpoint.js'
import type { InboundFilterKind, InboundFilterResponse } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { newline, header, icons, json, success, info, warning, formatRelativeTime } from '../../ui/output.js'

/** Every rule kind, and whether it takes a `--pattern`. */
export const INBOUND_FILTER_KINDS: Record<InboundFilterKind, { pattern: boolean; label: string }> = {
  browser_extensions: { pattern: false, label: 'Browser extensions' },
  web_crawlers: { pattern: false, label: 'Web crawlers' },
  localhost: { pattern: false, label: 'Localhost' },
  error_message: { pattern: true, label: 'Error message' },
  release: { pattern: true, label: 'Release' },
}

/** Parse a kind as typed on the command line (`error-message` or `error_message`). */
export function parseFilterKind(raw: string): InboundFilterKind {
  const kind = raw.trim().toLowerCase().replace(/-/g, '_')
  if (!(kind in INBOUND_FILTER_KINDS)) {
    throw new Error(
      `Unknown filter kind "${raw}". Use one of: ${Object.keys(INBOUND_FILTER_KINDS)
        .map((k) => k.replace(/_/g, '-'))
        .join(', ')}`,
    )
  }
  return kind as InboundFilterKind
}

/**
 * Check a kind and pattern before sending them, with the same rules the
 * server applies, so the mistake is explained without a round trip.
 */
export function checkFilterPattern(kind: InboundFilterKind, pattern: string | undefined): string | null {
  const trimmed = pattern?.trim() || undefined
  if (INBOUND_FILTER_KINDS[kind].pattern) {
    if (!trimmed) {
      throw new Error(`A ${kind.replace(/_/g, '-')} filter needs --pattern, for example "*ResizeObserver*"`)
    }
    return trimmed
  }
  if (trimmed) {
    throw new Error(`A ${kind.replace(/_/g, '-')} filter takes no --pattern`)
  }
  return null
}

/** The body of an update, or an error when it would change nothing. */
export function buildFilterUpdate(options: { pattern?: string; enable?: boolean; disable?: boolean }): {
  pattern?: string
  enabled?: boolean
} {
  if (options.enable && options.disable) {
    throw new Error('Use either --enable or --disable, not both')
  }
  const body: { pattern?: string; enabled?: boolean } = {}
  if (options.pattern !== undefined) body.pattern = options.pattern
  if (options.enable) body.enabled = true
  if (options.disable) body.enabled = false
  if (Object.keys(body).length === 0) {
    throw new Error('Nothing to change: pass --pattern, --enable or --disable')
  }
  return body
}

interface ProjectOptions {
  projectId: string
  json?: boolean
}

interface AddOptions extends ProjectOptions {
  kind: string
  pattern?: string
  disabled?: boolean
}

interface FilterOptions extends ProjectOptions {
  filterId: string
}

interface UpdateOptions extends FilterOptions {
  pattern?: string
  enable?: boolean
  disable?: boolean
}

function ids(options: { projectId: string; filterId?: string }): { projectId: number; filterId: number } | null {
  const projectId = parseInt(options.projectId, 10)
  const filterId = options.filterId === undefined ? 0 : parseInt(options.filterId, 10)
  if (isNaN(projectId) || isNaN(filterId)) {
    warning('Invalid project or filter ID')
    return null
  }
  return { projectId, filterId }
}

export function registerInboundFilterCommands(errors: Command): void {
  const filters = errors
    .command('filters')
    .description('Drop noisy error events before they are stored (browser extensions, crawlers, localhost, message or release patterns)')

  endpoint(filters, getErrorInboundFilter, {
    name: 'get',
    description: "Show one of a project's inbound filters",
    path: { project_id: 'project', filter_id: 'int' },
  })

  filters
    .command('list')
    .description("List a project's inbound filters and how many events each dropped")
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listFiltersAction)

  filters
    .command('add')
    .description('Add an inbound filter')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--kind <kind>', 'browser-extensions, web-crawlers, localhost, error-message or release')
    .option('--pattern <glob>', 'For error-message and release: a case-insensitive glob, * and ? wildcards')
    .option('--disabled', 'Create the filter switched off')
    .option('--json', 'Output in JSON format')
    .action(addFilterAction)

  filters
    .command('update')
    .description("Change a filter's pattern, or switch it on or off")
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--filter-id <id>', 'Filter ID')
    .option('--pattern <glob>', 'New pattern')
    .option('--enable', 'Switch the filter on')
    .option('--disable', 'Switch the filter off')
    .option('--json', 'Output in JSON format')
    .action(updateFilterAction)

  filters
    .command('remove')
    .alias('delete')
    .description('Delete an inbound filter; matching events are stored again')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--filter-id <id>', 'Filter ID')
    .action(removeFilterAction)
}

async function listFiltersAction(options: ProjectOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const parsed = ids(options)
  if (!parsed) return

  const result = await withSpinner('Loading inbound filters...', async () => {
    const { data, error } = await listErrorInboundFilters({ client, path: { project_id: parsed.projectId } })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  newline()
  header(`${icons.info} Inbound filters (${result.filters.length} of ${result.limit})`)
  if (result.filters.length === 0) {
    info('No filters: every error the SDKs send is stored. Add one with `temps errors filters add`.')
    newline()
    return
  }
  printTable<InboundFilterResponse>(
    result.filters,
    [
      { header: 'ID', accessor: (f) => f.id },
      { header: 'Drops', accessor: (f) => INBOUND_FILTER_KINDS[f.kind]?.label ?? f.kind },
      { header: 'Pattern', accessor: (f) => f.pattern ?? '-' },
      { header: 'On', accessor: (f) => (f.enabled ? 'yes' : 'no') },
      { header: 'Dropped', accessor: (f) => f.dropped_count, align: 'right' },
      {
        header: 'Last drop',
        accessor: (f) => (f.last_dropped_at ? formatRelativeTime(f.last_dropped_at) : '-'),
      },
    ],
    { style: 'minimal' },
  )
  newline()
}

async function addFilterAction(options: AddOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const parsed = ids(options)
  if (!parsed) return
  let kind: InboundFilterKind
  let pattern: string | null
  try {
    kind = parseFilterKind(options.kind)
    pattern = checkFilterPattern(kind, options.pattern)
  } catch (e) {
    warning(e instanceof Error ? e.message : String(e))
    return
  }

  const created = await withSpinner('Adding inbound filter...', async () => {
    const { data, error } = await createErrorInboundFilter({
      client,
      path: { project_id: parsed.projectId },
      body: { kind, pattern, enabled: !options.disabled },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(created)
    return
  }
  success(
    `Filter #${created.id} added: ${INBOUND_FILTER_KINDS[created.kind].label}${created.pattern ? ` ${created.pattern}` : ''}${created.enabled ? '' : ' (off)'}`,
  )
  info('It applies from the next event. Dropped events are not stored and cannot be recovered.')
}

async function updateFilterAction(options: UpdateOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const parsed = ids(options)
  if (!parsed) return
  let body: { pattern?: string; enabled?: boolean }
  try {
    body = buildFilterUpdate(options)
  } catch (e) {
    warning(e instanceof Error ? e.message : String(e))
    return
  }

  const updated = await withSpinner(`Updating inbound filter #${parsed.filterId}...`, async () => {
    const { data, error } = await updateErrorInboundFilter({
      client,
      path: { project_id: parsed.projectId, filter_id: parsed.filterId },
      body,
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(updated)
    return
  }
  success(`Filter #${updated.id} ${updated.enabled ? 'on' : 'off'}${updated.pattern ? `, pattern ${updated.pattern}` : ''}`)
}

async function removeFilterAction(options: FilterOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const parsed = ids(options)
  if (!parsed) return

  await withSpinner(`Deleting inbound filter #${parsed.filterId}...`, async () => {
    const { error } = await deleteErrorInboundFilter({
      client,
      path: { project_id: parsed.projectId, filter_id: parsed.filterId },
    })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Filter #${parsed.filterId} deleted; matching events are stored again`)
}
