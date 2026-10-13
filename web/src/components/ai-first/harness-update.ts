// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Presentation helpers for the harness update action bar. */

export interface HarnessUpdateView {
  installed_version?: string | null
  latest_version?: string | null
  update_available: boolean
  updating: boolean
}

/** How long a cached check stays fresh in the console before a re-check. */
export const HARNESS_UPDATE_STALE_MS = 60 * 60 * 1000

/** Poll quickly while an update runs, otherwise re-check hourly. */
export function harnessUpdateRefetchInterval(
  status: Pick<HarnessUpdateView, 'updating'> | undefined
): number {
  return status?.updating ? 3_000 : HARNESS_UPDATE_STALE_MS
}

/** The bar is shown for an available update or one in progress. */
export function shouldShowHarnessUpdate(
  status: HarnessUpdateView | undefined
): boolean {
  return Boolean(status && (status.update_available || status.updating))
}

/** "2.1.263 → 2.1.296", or the available version alone when unknown. */
export function harnessVersionChange(status: HarnessUpdateView): string {
  const { installed_version: from, latest_version: to } = status
  if (from && to) return `${from} → ${to}`
  return to ?? from ?? ''
}

export function harnessUpdateHeadline(
  providerName: string,
  status: HarnessUpdateView
): string {
  if (status.updating) return `Updating ${providerName}…`
  return `${providerName} update available`
}

export function harnessUpdateDetail(
  providerName: string,
  status: HarnessUpdateView
): string {
  const change = harnessVersionChange(status)
  const versions = change ? ` (${change})` : ''
  if (status.updating) {
    return `Installing the latest ${providerName}${versions} in your workspace. This can take a minute.`
  }
  return `A newer ${providerName}${versions} is available. Models released after your installed version won't be offered until you update.`
}
