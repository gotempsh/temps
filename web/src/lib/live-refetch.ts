// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Freshness policy for lists that change without the user touching them
 * (a deployment started by a webhook, a scheduled backup, a variable written
 * through the API or CLI).
 *
 * Two mechanisms, both bounded:
 *  - returning to the tab refetches whatever has gone stale
 *    ({@link refetchOnFocusWhenStale}), app-wide;
 *  - a list that is on screen polls on a slow idle cadence, and on a fast one
 *    while a row is still in progress ({@link liveListInterval}). React Query
 *    pauses interval polling while the tab is hidden, so a forgotten tab costs
 *    nothing.
 */

/** Poll cadence while at least one row is still building / running. */
export const ACTIVE_LIST_POLL_MS = 3_000

/** Poll cadence for a list with nothing in progress. */
export const IDLE_LIST_POLL_MS = 15_000

/**
 * Idle cadence for a list whose endpoint returns the whole history rather than
 * a bounded page (a source's backups): each poll costs O(total), so it backs
 * off further than {@link IDLE_LIST_POLL_MS}.
 */
export const UNBOUNDED_LIST_IDLE_POLL_MS = 30_000

/**
 * A tab regaining focus refetches only data older than this. Quick alt-tabs
 * (and focus events fired by dialogs or iframes) therefore cost nothing, while
 * a list looked at again after a pause is never older than this.
 */
export const FOCUS_REFETCH_MIN_AGE_MS = 10_000

/** Poll interval for a visible list: fast while work is in progress. */
export function liveListInterval(hasInProgressRows: boolean): number {
  return hasInProgressRows ? ACTIVE_LIST_POLL_MS : IDLE_LIST_POLL_MS
}

const IN_PROGRESS_BACKUP_STATES = new Set(['pending', 'running'])

/**
 * Poll interval for a source's Recent Backups list.
 *
 * A backup started by a schedule, the CLI or another operator has to appear
 * without a reload, and a running one has to reach its final state. The
 * listing is not paged server-side, so the idle cadence is the slower
 * {@link UNBOUNDED_LIST_IDLE_POLL_MS}; polling stays fast only while something
 * is actually running.
 */
export function sourceBackupsRefetchInterval(
  backups: readonly { state: string }[] | undefined
): number {
  const inProgress =
    backups?.some((backup) => IN_PROGRESS_BACKUP_STATES.has(backup.state)) ??
    false
  return inProgress ? ACTIVE_LIST_POLL_MS : UNBOUNDED_LIST_IDLE_POLL_MS
}

/**
 * `refetchOnWindowFocus` policy for the app-wide QueryClient default.
 *
 * React Query only refetches on focus when the query is stale; with the
 * default `staleTime: 0` that is every query, so the age gate here is what
 * keeps a burst of focus events from fanning out into a burst of requests.
 * Queries that never fetched (no `dataUpdatedAt`) are left to their own
 * mount-time fetch.
 */
export function refetchOnFocusWhenStale(
  query: { state: { dataUpdatedAt: number } },
  now: number = Date.now()
): boolean {
  const { dataUpdatedAt } = query.state
  if (!dataUpdatedAt) return false
  return now - dataUpdatedAt >= FOCUS_REFETCH_MIN_AGE_MS
}
