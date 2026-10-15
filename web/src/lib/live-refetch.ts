// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Freshness policy for lists that change without the user touching them
 * (a deployment started by a webhook, a scheduled backup, a variable written
 * through the API or CLI).
 *
 * Two mechanisms, both bounded and both opt-in per query:
 *  - returning to the tab refetches what has gone stale
 *    ({@link refetchOnFocusWhenStale});
 *  - a list that is on screen polls on a slow idle cadence, and on a fast one
 *    while a row is still in progress ({@link liveListInterval}). React Query
 *    pauses interval polling while the tab is hidden, so a forgotten tab costs
 *    nothing.
 *
 * Neither is an app-wide default: several settings editors reset their form
 * from the fetched object, so a blanket focus refetch would discard unsaved
 * drafts. A list opts in only when nothing on its page is seeded from it.
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
 * Past this many rows an unbounded listing stops idle polling altogether: it
 * still refreshes on focus, on mutation, and when a followed backup settles,
 * but a heavy history is not re-read every {@link UNBOUNDED_LIST_IDLE_POLL_MS}.
 */
export const UNBOUNDED_LIST_MAX_IDLE_POLLED_ROWS = 200

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

/** Whether a backup row is still being worked on. */
export function isBackupInProgress(backup: { state: string }): boolean {
  return IN_PROGRESS_BACKUP_STATES.has(backup.state)
}

/**
 * Ids of the backups in a source's listing that are still running.
 *
 * The listing endpoint returns the whole retained history, so re-reading it on
 * a fast cadence would cost O(total rows) per poll. Instead the running ones are
 * followed individually (O(running)) and the listing is refetched once when one
 * of them settles. S3-scan entries have no DB row (empty id) and are skipped.
 */
export function inProgressBackupIds(
  backups: readonly { backup_id: string; state: string }[] | undefined
): string[] {
  return (backups ?? [])
    .filter((backup) => backup.backup_id && isBackupInProgress(backup))
    .map((backup) => backup.backup_id)
}

/**
 * Poll interval for a source's Recent Backups list itself.
 *
 * A backup started by a schedule, the CLI or another operator has to appear
 * without a reload. The listing is not paged server-side, so the cadence is the
 * slow {@link UNBOUNDED_LIST_IDLE_POLL_MS}, and it stops past
 * {@link UNBOUNDED_LIST_MAX_IDLE_POLLED_ROWS} rows (it still refreshes on focus
 * and when a followed backup settles). Following a running backup is done per
 * record, never by speeding this up.
 */
export function sourceBackupsRefetchInterval(
  backups: readonly unknown[] | undefined
): number | false {
  if ((backups?.length ?? 0) > UNBOUNDED_LIST_MAX_IDLE_POLLED_ROWS) return false
  return UNBOUNDED_LIST_IDLE_POLL_MS
}

/**
 * `refetchOnWindowFocus` policy for lists that opt in to refreshing on focus.
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
