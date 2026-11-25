// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/**
 * Pure helpers for the AI models page (ADR 113): the runtime model catalog
 * and the coding-agent CLIs.
 */

export interface CatalogSettingsLike {
  source_url?: string | null
  effective_source_url: string
  auto_refresh: boolean
  refresh_interval_hours: number
  discover_provider_models: boolean
}

/** Where the catalog comes from, in words: the Temps catalog or a host. */
export function catalogSourceLabel(settings: CatalogSettingsLike): string {
  if (!settings.source_url) return 'Temps catalog'
  try {
    return new URL(settings.source_url).host
  } catch {
    return settings.source_url
  }
}

export const MIN_REFRESH_HOURS = 1
export const MAX_REFRESH_HOURS = 168

export const catalogSettingsSchema = z.object({
  sourceUrl: z
    .string()
    .trim()
    .max(2048, 'The URL is longer than 2048 characters')
    .refine(
      (value) => value === '' || /^https:\/\/[^\s/]+/i.test(value),
      'Use an https:// URL, or leave blank for the Temps catalog'
    )
    .refine(
      (value) => !/[?#]/.test(value) && !/^[a-z]+:\/\/[^/]*@/i.test(value),
      'A catalog is a public document: the URL cannot carry a query, fragment or credentials'
    ),
  refreshIntervalHours: z
    .string()
    .trim()
    .refine((value) => /^\d+$/.test(value), 'Enter a whole number of hours')
    .refine((value) => {
      const hours = Number(value)
      return hours >= MIN_REFRESH_HOURS && hours <= MAX_REFRESH_HOURS
    }, `Between ${MIN_REFRESH_HOURS} and ${MAX_REFRESH_HOURS} hours`),
  autoRefresh: z.boolean(),
  discoverProviderModels: z.boolean(),
})

export type CatalogSettingsForm = z.infer<typeof catalogSettingsSchema>

export function catalogSettingsToForm(
  settings: CatalogSettingsLike
): CatalogSettingsForm {
  return {
    sourceUrl: settings.source_url ?? '',
    refreshIntervalHours: String(settings.refresh_interval_hours),
    autoRefresh: settings.auto_refresh,
    discoverProviderModels: settings.discover_provider_models,
  }
}

/** The PATCH body; a blank URL goes back to the Temps catalog. */
export function catalogFormToUpdate(values: CatalogSettingsForm) {
  return {
    source_url: values.sourceUrl.trim(),
    refresh_interval_hours: Number(values.refreshIntervalHours.trim()),
    auto_refresh: values.autoRefresh,
    discover_provider_models: values.discoverProviderModels,
  }
}

/** `$2.50` per 1M tokens, or an em dash when the catalog has no price. */
export function formatPerMillion(value: number | null | undefined): string {
  if (value === null || value === undefined) return '—'
  if (value === 0) return '$0'
  const digits = value < 0.1 ? 4 : 2
  return `$${value.toFixed(digits).replace(/(\.\d*?[1-9])0+$|\.0+$/, '$1')}`
}

/**
 * The page to show for `page` once the list holds `total` items: the last
 * page that exists when `page` is past it (a refresh shrank the list), never
 * below 1.
 */
export function clampPage(
  page: number,
  total: number,
  pageSize: number
): number {
  const lastPage = Math.max(1, Math.ceil(total / pageSize))
  return Math.min(Math.max(1, page), lastPage)
}

/** `200K`, `1M`: a context window at a glance. */
export function formatTokens(value: number | null | undefined): string {
  if (value === null || value === undefined) return '—'
  if (value >= 1_000_000) {
    return `${Number((value / 1_000_000).toFixed(1))}M`
  }
  if (value >= 1_000) return `${Math.round(value / 1_000)}K`
  return String(value)
}

export interface AgentReleaseLike {
  latest_version?: string | null
  checked_at?: string | null
  error?: string | null
}

export interface WorkspaceHarnessLike {
  installed_version?: string | null
  latest_version?: string | null
  update_available: boolean
  updating: boolean
  checked_at?: string | null
  detail?: string | null
}

export type AgentVersionState =
  | { kind: 'updating' }
  /** Never read: reading it would start or wake the workspace. */
  | { kind: 'unchecked' }
  | { kind: 'behind'; installed: string; latest: string }
  | { kind: 'current'; installed: string }
  | { kind: 'unknown'; reason: string; installed?: string }

/**
 * The newer of two release versions; either may be missing. A version that
 * does not parse loses to one that does.
 */
export function newestVersion(
  a: string | null | undefined,
  b: string | null | undefined
): string | null {
  if (!a) return b ?? null
  if (!b) return a
  return isNewerVersion(b, a) ? b : a
}

/**
 * The latest release by the most recent of the two checks: the workspace's
 * own (cached for hours) and the registry's. A release "Check for releases"
 * just found is not hidden behind a stale workspace answer, and a workspace
 * just updated is not told about a release npm has since withdrawn. Without
 * both timestamps, the newer version.
 */
export function latestReleaseVersion(
  release: AgentReleaseLike,
  workspace: WorkspaceHarnessLike | null | undefined
): string | null {
  const fromWorkspace = workspace?.latest_version ?? null
  const fromRegistry = release.latest_version ?? null
  if (!fromWorkspace || !fromRegistry) return fromWorkspace ?? fromRegistry
  const workspaceAt = Date.parse(workspace?.checked_at ?? '')
  const registryAt = Date.parse(release.checked_at ?? '')
  if (Number.isFinite(workspaceAt) && Number.isFinite(registryAt)) {
    return workspaceAt >= registryAt ? fromWorkspace : fromRegistry
  }
  return newestVersion(fromWorkspace, fromRegistry)
}

/**
 * How the CLI in the caller's workspace compares with the latest release
 * (see `latestReleaseVersion`). With no latest version at all the state is
 * unknown, never up to date.
 */
export function agentVersionState(
  release: AgentReleaseLike,
  workspace: WorkspaceHarnessLike | null | undefined,
  workspaceError?: string | null
): AgentVersionState {
  if (workspace?.updating) return { kind: 'updating' }
  if (
    workspace &&
    !workspace.checked_at &&
    !workspace.installed_version &&
    !workspace.detail &&
    !workspaceError
  ) {
    return { kind: 'unchecked' }
  }
  const installed = workspace?.installed_version ?? null
  const latest = latestReleaseVersion(release, workspace)
  if (!installed) {
    return {
      kind: 'unknown',
      reason:
        workspaceError ??
        workspace?.detail ??
        'The version in your workspace has not been read yet.',
    }
  }
  // The workspace's own flag holds only for the release it was computed
  // against: once a newer registry check says otherwise (a withdrawn
  // release), compare the versions instead.
  if (
    workspace?.update_available &&
    latest &&
    latest === workspace.latest_version
  ) {
    return { kind: 'behind', installed, latest }
  }
  if (!latest) {
    return {
      kind: 'unknown',
      installed,
      reason:
        workspace?.detail ??
        workspaceError ??
        release.error ??
        'The latest release could not be looked up.',
    }
  }
  if (isNewerVersion(latest, installed)) {
    return { kind: 'behind', installed, latest }
  }
  return { kind: 'current', installed }
}

function versionCore(version: string): [number, number, number] | null {
  const match = /^v?(\d+)\.(\d+)\.(\d+)$/.exec(version.split(/[-+]/)[0] ?? '')
  return match ? [Number(match[1]), Number(match[2]), Number(match[3])] : null
}

/** `latest` is a strictly newer release than `installed`. */
export function isNewerVersion(latest: string, installed: string): boolean {
  const a = versionCore(latest)
  const b = versionCore(installed)
  if (!a || !b) return false
  for (let index = 0; index < 3; index += 1) {
    if (a[index] !== b[index]) return a[index] > b[index]
  }
  return false
}

export interface Outcome {
  level: 'success' | 'warning' | 'error'
  message: string
}

/**
 * What to tell the operator after Update returned. The installer exiting
 * zero is not enough: the version the workspace reports afterwards decides,
 * compared with the newest release known from the workspace's own lookup or
 * the registry check (the workspace lookup can fail, e.g. without npm).
 */
export function updateOutcome(
  name: string,
  release: AgentReleaseLike,
  status: WorkspaceHarnessLike
): Outcome {
  const installed = status.installed_version ?? null
  if (!installed) {
    return {
      level: 'warning',
      message: `${name} was installed, but its version could not be read${
        status.detail ? `: ${status.detail}` : ''
      }`,
    }
  }
  const state = agentVersionState(release, status)
  if (state.kind === 'behind') {
    return {
      level: 'warning',
      message: `${name} still reports ${installed} (latest ${state.latest}); another copy may come first on PATH`,
    }
  }
  if (state.kind !== 'current') {
    return {
      level: 'warning',
      message: `${name} now reports ${installed}, but the latest release could not be looked up to confirm it is current`,
    }
  }
  return { level: 'success', message: `${name} updated to ${installed}` }
}

/**
 * What to tell the operator after Check for releases: the request succeeds
 * even when the registry lookup for an agent failed, which the row reports
 * in its `error`.
 */
export function releaseCheckOutcome(
  agents: Array<{ name: string; error?: string | null }>
): Outcome {
  const failed = agents.filter((agent) => agent.error)
  if (failed.length === 0) {
    return { level: 'success', message: 'Checked for new releases' }
  }
  if (failed.length === agents.length) {
    return {
      level: 'error',
      message: `Releases could not be looked up: ${failed[0]?.error ?? 'the registry did not answer'}`,
    }
  }
  return {
    level: 'warning',
    message: `Releases could not be looked up for ${failed
      .map((agent) => agent.name)
      .join(', ')}`,
  }
}
