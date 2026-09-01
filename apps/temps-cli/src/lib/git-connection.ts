// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Shared git connection utilities for project creation and setup wizard.
 * Extracted from commands/projects/create.ts to avoid duplication.
 */
import { client } from './api-client.js'
import { getErrorMessage } from './api-client.js'
import {
  listConnections,
  listRepositoriesByConnection,
  syncRepositories,
  getRepositoryPresetLive,
  getRepositoryBranches,
} from '../api/sdk.gen.js'
import type {
  ConnectionResponse,
  RepositoryResponse,
  ProjectPresetResponse,
} from '../api/types.gen.js'
import { promptSearch, promptSelect, type SelectOption, type SearchOption } from '../ui/prompts.js'
import { startSpinner, succeedSpinner, failSpinner } from '../ui/spinner.js'
import { info, warning, newline, colors } from '../ui/output.js'

/**
 * Fetch all git connections and let the user select one.
 * If only one connection exists, auto-selects it.
 * Returns null if no connections are available.
 */
export async function selectGitConnection(): Promise<ConnectionResponse | null> {
  const spinner = startSpinner('Loading git connections...')

  const { data, error: apiError } = await listConnections({ client })

  if (apiError) {
    failSpinner('Failed to load git connections')
    throw new Error(getErrorMessage(apiError))
  }

  succeedSpinner('Git connections loaded')

  const connections = data?.connections || []

  if (connections.length === 0) {
    newline()
    warning('No git connections found.')
    info('Set up a git provider by running: temps providers add')
    return null
  }

  if (connections.length === 1) {
    const conn = connections[0]!
    info(`Using git connection: ${conn.account_name}`)
    return conn
  }

  newline()
  const choices: SelectOption<number>[] = connections.map((conn) => ({
    name: `${conn.account_name} (${conn.account_type})`,
    value: conn.id,
    description: conn.is_active ? 'Active' : 'Inactive',
  }))

  const selectedId = await promptSelect({
    message: 'Select git connection',
    choices,
  })

  return connections.find((c) => c.id === selectedId) || null
}

/**
 * Fetch repositories for a connection and let the user search/select one.
 * Auto-syncs if no repositories are found.
 */
export async function selectRepository(connectionId: number): Promise<RepositoryResponse | null> {
  const spinner = startSpinner('Loading repositories...')

  const { data, error: apiError } = await listRepositoriesByConnection({
    client,
    path: { connection_id: connectionId },
    query: { per_page: 100 },
  })

  if (apiError) {
    failSpinner('Failed to load repositories')
    throw new Error(getErrorMessage(apiError))
  }

  succeedSpinner('Repositories loaded')

  let repositories = data?.repositories || []

  // Auto-sync if no repositories found
  if (repositories.length === 0) {
    info('No repositories found. Syncing from provider...')
    const { error: syncError } = await syncRepositories({
      client,
      path: { connection_id: connectionId },
    })
    if (syncError) {
      throw new Error(getErrorMessage(syncError))
    }

    // Reload after sync
    const { data: reloadedData, error: reloadError } = await listRepositoriesByConnection({
      client,
      path: { connection_id: connectionId },
      query: { per_page: 100 },
    })

    if (reloadError) {
      throw new Error(getErrorMessage(reloadError))
    }

    repositories = reloadedData?.repositories || []

    if (repositories.length === 0) {
      warning('No repositories found after syncing. Check your Git provider permissions.')
      return null
    }
  }

  newline()

  // Build search choices from all repositories
  const choices: SearchOption<number>[] = repositories.map((repo) => ({
    name: `${repo.owner}/${repo.name}`,
    value: repo.id,
    description: [repo.language, repo.description?.slice(0, 60)].filter(Boolean).join(' • ') || undefined,
  }))

  info(`${repositories.length} repositories available. Type to search...`)
  newline()

  const selectedId = await promptSearch({
    message: 'Select repository',
    choices,
    pageSize: 15,
  })

  return repositories.find((r) => r.id === selectedId) || null
}

/**
 * Find a repository by owner/name in a connection's repository list.
 * Returns the repository if found, null otherwise.
 */
export async function findRepositoryByName(
  connectionId: number,
  owner: string,
  repoName: string
): Promise<RepositoryResponse | null> {
  // Search server-side rather than paging through everything. Without this the
  // call fetched an arbitrary first 100 repositories and matched client-side,
  // so on any organisation with more than 100 repos a perfectly valid --repo
  // was reported as "not found in connection" purely because it happened to
  // sort past the first page.
  const { data, error: apiError } = await listRepositoriesByConnection({
    client,
    path: { connection_id: connectionId },
    query: { per_page: 100, search: repoName },
  })

  if (apiError || !data?.repositories) {
    return null
  }

  const match = (r: RepositoryResponse) =>
    r.owner.toLowerCase() === owner.toLowerCase() && r.name.toLowerCase() === repoName.toLowerCase()

  const found = data.repositories.find(match)
  if (found) return found

  // Fall back to an unfiltered page for providers that ignore `search`, so this
  // is never worse than the previous behaviour.
  const { data: unfiltered } = await listRepositoriesByConnection({
    client,
    path: { connection_id: connectionId },
    query: { per_page: 100 },
  })
  return unfiltered?.repositories?.find(match) ?? null
}

/**
 * Fetch branches for a repository and let the user select one.
 * Falls back to the repository's default branch if loading fails.
 */
export async function selectBranch(
  connectionId: number,
  repository: RepositoryResponse
): Promise<string> {
  const spinner = startSpinner('Loading branches...')

  const { data, error: apiError } = await getRepositoryBranches({
    client,
    path: { owner: repository.owner, repo: repository.name },
    query: { connection_id: connectionId },
  })

  if (apiError || !data?.branches || data.branches.length === 0) {
    failSpinner('Could not load branches, using default')
    return repository.default_branch || 'main'
  }

  succeedSpinner('Branches loaded')

  const branches = data.branches

  // If only one branch, use it
  if (branches.length === 1) {
    info(`Using branch: ${branches[0]!.name}`)
    return branches[0]!.name
  }

  newline()

  const choices: SelectOption<string>[] = branches.map((branch) => ({
    name: branch.name,
    value: branch.name,
    description: branch.name === repository.default_branch ? 'Default branch' : undefined,
  }))

  // Put default branch first
  choices.sort((a, b) => {
    if (a.value === repository.default_branch) return -1
    if (b.value === repository.default_branch) return 1
    return 0
  })

  return await promptSelect({
    message: 'Select branch',
    choices,
    default: repository.default_branch,
  })
}

/**
 * Detect the preset for a repository by calling the API's live detection.
 * Falls back to listing all presets for manual selection if detection fails.
 */
export async function detectAndSelectPreset(
  repositoryId: number,
  branch: string
): Promise<{ preset: string; directory: string }> {
  const spinner = startSpinner('Detecting framework...')

  const { data: presetData, error: presetError } = await getRepositoryPresetLive({
    client,
    path: { repository_id: repositoryId },
    query: { branch },
  })

  let detectedPresets: ProjectPresetResponse[] = []
  if (!presetError && presetData?.presets) {
    detectedPresets = presetData.presets
  }

  succeedSpinner(
    detectedPresets.length > 0
      ? `Detected ${detectedPresets.length} framework(s)`
      : 'No frameworks detected'
  )

  return selectPresetFromDetected(detectedPresets)
}

/**
 * Present detected presets to the user for selection, with a fallback to browse all presets.
 */
export async function selectPresetFromDetected(
  detectedPresets: ProjectPresetResponse[]
): Promise<{ preset: string; directory: string }> {
  const { listPresets } = await import('../api/sdk.gen.js')

  // Load all available presets
  const { data: allPresetsData } = await listPresets({ client })
  const allPresets = allPresetsData?.presets || []

  newline()

  // Show detected presets first, then allow browsing all
  if (detectedPresets.length > 0) {
    const detectedChoices: SelectOption<string>[] = detectedPresets.map((p) => ({
      name: `${p.preset || 'unknown'} ${colors.muted(`(${p.path || '.'})`)}`,
      value: `${p.preset}::${p.path || '.'}`,
      description: 'Detected in repository',
    }))

    detectedChoices.push({
      name: colors.muted('Browse all frameworks...'),
      value: 'browse_all',
      description: 'Select from all available presets',
    })

    const selected = await promptSelect({
      message: 'Select framework',
      choices: detectedChoices,
    })

    if (selected !== 'browse_all') {
      const [preset, path] = selected.split('::')
      return { preset: preset!, directory: path || './' }
    }
  }

  // Show all presets
  if (allPresets.length === 0) {
    warning('No presets available')
    return { preset: 'custom', directory: './' }
  }

  const allChoices: SelectOption<string>[] = allPresets.map((p) => ({
    name: p.label,
    value: p.slug,
    description: p.description,
  }))

  allChoices.push({
    name: 'Custom / Dockerfile',
    value: 'dockerfile',
    description: 'Use a custom Dockerfile',
  })

  const preset = await promptSelect({
    message: 'Select framework',
    choices: allChoices,
  })

  // Ask for directory
  const { promptText } = await import('../ui/prompts.js')
  const directory = await promptText({
    message: 'Root directory (relative to repo)',
    default: './',
  })

  return { preset, directory: directory || './' }
}

/**
 * Fetch all git connections (non-interactive, for matching purposes).
 */
export async function fetchGitConnections(): Promise<ConnectionResponse[]> {
  const { data, error: apiError } = await listConnections({ client })
  if (apiError || !data?.connections) {
    return []
  }
  return data.connections
}
