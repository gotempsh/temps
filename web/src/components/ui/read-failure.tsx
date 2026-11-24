// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AlertTriangle, ShieldAlert } from 'lucide-react'
import { useEffect } from 'react'
import { Alert, AlertDescription, AlertTitle } from './alert'
import { Button } from './button'
import {
  isForbiddenRead,
  readFailureExplanation,
  readFailureServerDetail,
} from '@/lib/read-failure'
import { markReadFailureHandled } from '@/lib/route-query-failures'

const CACHED_NOTE = 'Showing last-known data. It is stale and may have changed.'

/**
 * Inline read recovery. Retry only replays a query, never a mutation.
 *
 * Render this in place of an empty state or a "not found" message whenever
 * the read itself failed: an empty list and a refused read look identical
 * otherwise, and the user is told they have nothing when they have no access.
 */
export function ReadFailure({
  resource,
  error,
  cached = false,
  onRetry,
  retrying = false,
  embedded = false,
  routeLevel = false,
}: {
  resource: string
  error: unknown
  cached?: boolean
  /** Omit when there is nothing to replay, e.g. a permission the role lacks. */
  onRetry?: () => void
  retrying?: boolean
  embedded?: boolean
  /**
   * Set by the route-level report itself. Every other use is the page
   * explaining its own failure, which tells the route-level report there is
   * nothing left to add.
   */
  routeLevel?: boolean
}) {
  useEffect(() => {
    if (!routeLevel) markReadFailureHandled(error)
  }, [error, routeLevel])
  const forbidden = isForbiddenRead(error)
  const serverDetail = readFailureServerDetail(error)
  const Icon = forbidden ? ShieldAlert : AlertTriangle
  const title = forbidden
    ? `${resource}: access denied`
    : `${resource} unavailable`
  const retryButton = onRetry && (
    <Button
      // Often rendered inside a form: a default submit button would
      // save the form instead of replaying the read.
      type="button"
      variant="outline"
      size="sm"
      className={routeLevel ? 'h-7 shrink-0 px-2.5 text-xs' : undefined}
      aria-disabled={retrying}
      onClick={() => {
        if (!retrying) onRetry()
      }}
    >
      {retrying ? 'Retrying…' : 'Retry'}
    </Button>
  )
  if (routeLevel) {
    // Shown above whatever page is open, so it takes one line: the page under
    // it is still the thing the user came for. The server's reason is the
    // most specific thing to show; the full explanation is on hover.
    const explanation = cached
      ? `${readFailureExplanation(error)} ${CACHED_NOTE}`
      : readFailureExplanation(error)
    const full = serverDetail ? `${explanation} ${serverDetail}` : explanation
    return (
      <div
        role="alert"
        data-read-failure={forbidden ? 'forbidden' : 'failed'}
        className="flex items-center gap-2 rounded-md border border-warning/50 bg-warning/10 px-3 py-1.5 text-sm text-warning-strong dark:border-warning"
      >
        <Icon className="size-4 shrink-0" aria-hidden="true" />
        <p className="min-w-0 flex-1 truncate" title={full}>
          <span className="font-medium">{title}</span>
          <span className="opacity-80"> · {serverDetail ?? explanation}</span>
          <span className="sr-only">
            {serverDetail ? ` ${explanation}` : ''}
          </span>
        </p>
        {retryButton}
      </div>
    )
  }
  return (
    <Alert
      variant="warning"
      data-read-failure={forbidden ? 'forbidden' : 'failed'}
      className={
        embedded ? 'rounded-none border-0 bg-transparent p-0' : undefined
      }
    >
      {!embedded && <Icon className="size-4" />}
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{readFailureExplanation(error)}</p>
        {serverDetail && (
          <p className="break-words">
            <span className="font-medium">Server response:</span> {serverDetail}
          </p>
        )}
        {cached && <p>{CACHED_NOTE}</p>}
        {retryButton}
      </AlertDescription>
    </Alert>
  )
}
