// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// `temps errors searches`: saved searches over error groups (ADR 111, D1.8),
// and `errors list --saved-search <id>` to run one.

import type { Command } from 'commander'
import { requireAuth } from '../../config/store.js'
import { setupClient, client, getErrorMessage } from '../../lib/api-client.js'
import {
  createErrorSavedSearch,
  deleteErrorSavedSearch,
  getErrorSavedSearch,
  listErrorSavedSearches,
} from '../../api/sdk.gen.js'
import type { SavedSearchFilters } from '../../api/types.gen.js'
import { withSpinner } from '../../ui/spinner.js'
import { printTable } from '../../ui/table.js'
import { header, info, json, success, warning, formatRelativeTime } from '../../ui/output.js'

/** The list flags a saved search can fill in. */
export interface SavedSearchListFlags {
  status?: string
  environmentId?: string
  assignee?: string
  search?: string
  sortBy?: string
  sortOrder?: string
}

/**
 * The `errors list` flags with a saved search applied: a flag given on the
 * command line wins over the saved value.
 */
export function applySavedSearch<T extends SavedSearchListFlags>(flags: T, saved: SavedSearchFilters): T {
  return {
    ...flags,
    status: flags.status ?? saved.status ?? undefined,
    environmentId: flags.environmentId ?? (saved.environment_id != null ? String(saved.environment_id) : undefined),
    assignee: flags.assignee ?? saved.assignee ?? undefined,
    search: flags.search ?? saved.search ?? undefined,
    sortBy: flags.sortBy ?? saved.sort_by ?? undefined,
    sortOrder: flags.sortOrder ?? saved.sort_order ?? undefined,
  }
}

/** The filters to save from `errors searches save` flags. */
export function filtersFromFlags(flags: SavedSearchListFlags): SavedSearchFilters {
  const environmentId = flags.environmentId ? parseInt(flags.environmentId, 10) : undefined
  if (flags.environmentId && (environmentId === undefined || isNaN(environmentId))) {
    throw new Error(`Invalid environment ID: ${flags.environmentId}`)
  }
  return {
    status: flags.status ?? null,
    environment_id: environmentId ?? null,
    assignee: flags.assignee ?? null,
    search: flags.search ?? null,
    sort_by: flags.sortBy ?? null,
    sort_order: flags.sortOrder ?? null,
  }
}

/** One line summing up what a saved search filters on. */
export function describeFilters(filters: SavedSearchFilters): string {
  const parts = [
    filters.status ? `status ${filters.status}` : 'any status',
    filters.assignee ? `assignee ${filters.assignee}` : null,
    filters.environment_id != null ? `environment ${filters.environment_id}` : null,
    filters.search ? `"${filters.search}"` : null,
    `by ${filters.sort_by ?? 'last_seen'} ${filters.sort_order ?? 'desc'}`,
  ]
  return parts.filter(Boolean).join(', ')
}

/** Load a saved search for `errors list --saved-search`. */
export async function loadSavedSearch(projectId: number, raw: string): Promise<SavedSearchFilters> {
  const savedSearchId = parseInt(raw, 10)
  if (isNaN(savedSearchId)) {
    throw new Error(`Invalid saved search ID: ${raw}`)
  }
  const { data, error } = await getErrorSavedSearch({
    client,
    path: { project_id: projectId, saved_search_id: savedSearchId },
  })
  if (error || !data) {
    throw new Error(getErrorMessage(error))
  }
  return data.filters
}

interface ProjectOptions {
  projectId: string
  json?: boolean
}

interface SaveOptions extends ProjectOptions, SavedSearchListFlags {
  name: string
  shared?: boolean
}

interface DeleteOptions extends ProjectOptions {
  id: string
}

function projectIdOf(options: ProjectOptions): number | null {
  const projectId = parseInt(options.projectId, 10)
  if (isNaN(projectId)) {
    warning('Invalid project ID')
    return null
  }
  return projectId
}

export function registerSavedSearchCommands(errors: Command): void {
  const searches = errors.command('searches').description('Saved searches over error groups')

  searches
    .command('list')
    .alias('ls')
    .description('List your saved searches and the ones shared with the project')
    .requiredOption('--project-id <id>', 'Project ID')
    .option('--json', 'Output in JSON format')
    .action(listAction)

  searches
    .command('save')
    .description('Save a set of list filters under a name')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--name <name>', 'Name, unique among your saved searches in the project')
    .option('--shared', 'Share with everyone who can open the project')
    .option('--status <status>', 'unresolved, resolved or ignored')
    .option('--environment-id <id>', 'An environment of the project')
    .option('--assignee <who>', 'me, none, user:<id> or team:<id>')
    .option('--search <text>', 'Text in the title, type or message')
    .option('--sort-by <field>', 'last_seen, first_seen, total_count or events_in_range')
    .option('--sort-order <order>', 'asc or desc')
    .option('--json', 'Output in JSON format')
    .action(saveAction)

  searches
    .command('delete')
    .description('Delete a saved search (yours, or a shared one as an administrator)')
    .requiredOption('--project-id <id>', 'Project ID')
    .requiredOption('--id <id>', 'Saved search ID')
    .action(deleteAction)
}

async function listAction(options: ProjectOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = projectIdOf(options)
  if (projectId === null) return
  const result = await withSpinner('Loading saved searches...', async () => {
    const { data, error } = await listErrorSavedSearches({
      client,
      path: { project_id: projectId },
      query: { page: 1, page_size: 100 },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  header('Saved searches')
  if (result.data.length === 0) {
    info('None yet. Save one with: temps errors searches save --project-id <id> --name "Open and mine" --status unresolved --assignee me')
    return
  }
  printTable(result.data, [
    { header: 'ID', accessor: (row) => String(row.id) },
    { header: 'Name', accessor: (row) => row.name },
    { header: 'Owner', accessor: (row) => (row.is_owner ? 'you' : (row.owner_name ?? '-')) },
    { header: 'Shared', accessor: (row) => (row.shared ? 'yes' : 'no') },
    { header: 'Filters', accessor: (row) => describeFilters(row.filters) },
    { header: 'Updated', accessor: (row) => formatRelativeTime(row.updated_at) },
  ])
  info('Run one with: temps errors list --project-id <id> --saved-search <ID>')
}

async function saveAction(options: SaveOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = projectIdOf(options)
  if (projectId === null) return
  let filters: SavedSearchFilters
  try {
    filters = filtersFromFlags(options)
  } catch (e) {
    warning(e instanceof Error ? e.message : String(e))
    return
  }
  const result = await withSpinner('Saving search...', async () => {
    const { data, error } = await createErrorSavedSearch({
      client,
      path: { project_id: projectId },
      body: { name: options.name, shared: options.shared ?? false, filters },
    })
    if (error || !data) throw new Error(getErrorMessage(error))
    return data
  })
  if (options.json) {
    json(result)
    return
  }
  success(`Saved search #${result.id} "${result.name}"${result.shared ? ' (shared)' : ''}`)
}

async function deleteAction(options: DeleteOptions): Promise<void> {
  await requireAuth()
  await setupClient()
  const projectId = projectIdOf(options)
  if (projectId === null) return
  const savedSearchId = parseInt(options.id, 10)
  if (isNaN(savedSearchId)) {
    warning('Invalid saved search ID')
    return
  }
  await withSpinner('Deleting saved search...', async () => {
    const { error } = await deleteErrorSavedSearch({
      client,
      path: { project_id: projectId, saved_search_id: savedSearchId },
    })
    if (error) throw new Error(getErrorMessage(error))
  })
  success(`Saved search #${savedSearchId} deleted`)
}
