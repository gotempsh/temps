// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'

/**
 * Pure helpers for the AI models page (ADR 112): the runtime model catalog
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
  error?: string | null
}

export interface WorkspaceHarnessLike {
  installed_version?: string | null
  latest_version?: string | null
  update_available: boolean
  updating: boolean
  detail?: string | null
}

export type AgentVersionState =
  | { kind: 'updating' }
  | { kind: 'behind'; installed: string; latest: string }
  | { kind: 'current'; installed: string }
  | { kind: 'unknown'; reason: string }

/**
 * How the CLI in the caller's workspace compares with the latest release.
 * The workspace's own check wins; the registry's latest fills in when the
 * workspace could not look it up.
 */
export function agentVersionState(
  release: AgentReleaseLike,
  workspace: WorkspaceHarnessLike | null | undefined,
  workspaceError?: string | null
): AgentVersionState {
  if (workspace?.updating) return { kind: 'updating' }
  const installed = workspace?.installed_version ?? null
  const latest = workspace?.latest_version ?? release.latest_version ?? null
  if (!installed) {
    return {
      kind: 'unknown',
      reason:
        workspaceError ??
        workspace?.detail ??
        'The version in your workspace has not been read yet.',
    }
  }
  if (workspace?.update_available && latest) {
    return { kind: 'behind', installed, latest }
  }
  if (latest && isNewerVersion(latest, installed)) {
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
