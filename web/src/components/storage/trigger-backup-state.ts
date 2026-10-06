// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * State derivation for the Trigger Backup dialog's storage-destination list.
 *
 * The dialog used to read only `data`/`isLoading` from the S3-source query,
 * so a failed read (403, 500, network) looked exactly like a successful
 * empty list and sent the operator off to create a destination that already
 * existed. Everything that decides which state the dialog is in, and whether
 * a backup may be submitted, lives here as pure functions so every branch is
 * unit-testable without rendering the dialog.
 */

import { extractProblemDetails } from '@/utils/errorHandling'

/** Headline for any failed read of the destination list. */
export const DESTINATIONS_LOAD_FAILED_MESSAGE =
  'Could not load backup destinations. Your configured destinations may still be available.'

/** Headline when a refresh fails but a previously loaded list is still shown. */
export const DESTINATIONS_REFRESH_FAILED_MESSAGE =
  'Could not refresh backup destinations. Showing the last loaded list.'

export const DESTINATIONS_PERMISSION_EXPLANATION =
  'You may not have permission to view backup destinations. Ask an administrator for access.'

export const DESTINATIONS_UNAVAILABLE_EXPLANATION =
  'The server could not be reached or is temporarily unavailable.'

export const SELECTED_DESTINATION_MISSING_MESSAGE =
  'The selected destination is no longer available. Choose another destination.'

/**
 * Why the destination list could not be read.
 *
 * - `permission`: 401/403 -- retrying will not help; an administrator must.
 * - `unavailable`: network failure, 5xx, or a non-JSON error body (typically
 *   a proxy page in front of an unreachable API).
 * - `unknown`: any other failure; only the server's own detail is shown.
 */
export type DestinationFailureReason = 'permission' | 'unavailable' | 'unknown'

export interface DestinationFailure {
  reason: DestinationFailureReason
  /** HTTP status, when the error body reports one. */
  status?: number
  /** Sentence explaining the reason, when the reason is known. */
  explanation?: string
  /** The server's Problem `detail`, when present. */
  detail?: string
}

/** The minimum a destination needs for state and selection decisions. */
export interface DestinationLike {
  id: number
}

export type DestinationState<T extends DestinationLike> =
  /** Initial read in flight (or not started): availability is unknown. */
  | { kind: 'loading' }
  /** Read failed and nothing usable is cached: availability is unknown. */
  | { kind: 'error'; failure: DestinationFailure }
  /** A refresh failed, but a previously loaded non-empty list is cached. */
  | { kind: 'stale'; destinations: T[]; failure: DestinationFailure }
  /** The server successfully reported that no destinations exist. */
  | { kind: 'empty' }
  /** The server successfully returned at least one destination. */
  | { kind: 'ready'; destinations: T[] }

export interface DestinationQuerySnapshot<T extends DestinationLike> {
  data: T[] | undefined
  error: unknown
  isPending: boolean
  isError: boolean
}

const NETWORK_ERROR_PATTERNS = [
  'failed to fetch',
  'networkerror',
  'network error',
  'load failed',
  'network request failed',
]

function isNetworkError(error: unknown): boolean {
  if (error instanceof TypeError) return true
  if (!error || typeof error !== 'object') return false
  const { message, name } = error as { message?: unknown; name?: unknown }
  if (name === 'NetworkError') return true
  if (typeof message !== 'string') return false
  const lowered = message.toLowerCase()
  return NETWORK_ERROR_PATTERNS.some((pattern) => lowered.includes(pattern))
}

/**
 * Classify a failed destination-list read.
 *
 * The generated client throws the parsed Problem body for HTTP errors, the
 * raw response text when the body is not JSON, and the `fetch` `TypeError`
 * for network failures, so each shape is handled here. A Problem body is not
 * guaranteed to carry `status` (proxies and older handlers omit it), so the
 * RFC 7807 `title` is used as a fallback for the permission case.
 */
export function classifyDestinationError(error: unknown): DestinationFailure {
  if (isNetworkError(error)) {
    return {
      reason: 'unavailable',
      explanation: DESTINATIONS_UNAVAILABLE_EXPLANATION,
    }
  }

  // A non-JSON error body: the request reached *something*, but not an API
  // handler that could answer in Problem Details.
  if (typeof error === 'string') {
    return {
      reason: 'unavailable',
      explanation: DESTINATIONS_UNAVAILABLE_EXPLANATION,
    }
  }

  const problem = extractProblemDetails(error)
  const status =
    typeof problem?.status === 'number' ? problem.status : undefined
  const detail =
    typeof problem?.detail === 'string' && problem.detail.trim().length > 0
      ? problem.detail
      : undefined
  const title = problem?.title

  const isPermission =
    status === 401 ||
    status === 403 ||
    (status === undefined &&
      (title === 'Forbidden' || title === 'Unauthorized'))
  if (isPermission) {
    return {
      reason: 'permission',
      status,
      explanation: DESTINATIONS_PERMISSION_EXPLANATION,
      detail,
    }
  }

  if (status !== undefined && status >= 500 && status < 600) {
    return {
      reason: 'unavailable',
      status,
      explanation: DESTINATIONS_UNAVAILABLE_EXPLANATION,
      detail,
    }
  }

  return { reason: 'unknown', status, detail }
}

/**
 * React Query retry policy for the destination list: a permission refusal
 * will not change on retry, so surface it immediately instead of holding the
 * dialog in its loading state through the default back-off. Everything else
 * keeps React Query's default of three retries.
 */
export function shouldRetryDestinationRead(
  failureCount: number,
  error: unknown
): boolean {
  if (classifyDestinationError(error).reason === 'permission') return false
  return failureCount < 3
}

/**
 * Derive the dialog's destination state from the query snapshot.
 *
 * Cached data wins over a later error: React Query keeps the last
 * successful `data` when a refetch fails, and that list is still the best
 * information available, so it stays visible (as `stale`) along with the
 * user's selection. A cached *empty* list with a failed refresh is reported
 * as `error`, not `empty`: inviting the user to create a destination needs a
 * read that currently succeeds.
 */
export function deriveDestinationState<T extends DestinationLike>(
  snapshot: DestinationQuerySnapshot<T>
): DestinationState<T> {
  const { data, error, isError } = snapshot

  if (data !== undefined) {
    if (isError) {
      const failure = classifyDestinationError(error)
      return data.length > 0
        ? { kind: 'stale', destinations: data, failure }
        : { kind: 'error', failure }
    }
    return data.length > 0
      ? { kind: 'ready', destinations: data }
      : { kind: 'empty' }
  }

  if (isError) {
    return { kind: 'error', failure: classifyDestinationError(error) }
  }

  // `isPending` without data, or a disabled query that has not run yet:
  // availability is unknown either way.
  return { kind: 'loading' }
}

export type SelectionStatus = 'none' | 'valid' | 'missing'

/** Whether `selectedId` is one of `destinations`. */
export function selectionStatus(
  destinations: readonly DestinationLike[],
  selectedId: number | undefined
): SelectionStatus {
  if (selectedId === undefined) return 'none'
  return destinations.some((destination) => destination.id === selectedId)
    ? 'valid'
    : 'missing'
}

/** The destinations a selection can be checked against, if any are known. */
export function knownDestinations<T extends DestinationLike>(
  state: DestinationState<T>
): T[] | undefined {
  return state.kind === 'ready' || state.kind === 'stale'
    ? state.destinations
    : undefined
}

/**
 * Whether a backup may be submitted to `selectedId`.
 *
 * Submission is blocked while availability is unknown (loading, or an error
 * with nothing cached), when no destination exists, and when the selection
 * is not in the list the dialog is showing. A selection that is still in a
 * cached list after a failed refresh remains submittable: the server
 * re-validates the id, and blocking a backup on a transient list refresh
 * failure would be worse than a clear server-side refusal.
 */
export function canSubmitBackup<T extends DestinationLike>(
  state: DestinationState<T>,
  selectedId: number | undefined
): boolean {
  const destinations = knownDestinations(state)
  if (!destinations) return false
  return selectionStatus(destinations, selectedId) === 'valid'
}
