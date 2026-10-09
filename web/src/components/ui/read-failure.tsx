// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { AlertTriangle, ShieldAlert } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from './alert'
import { Button } from './button'
import {
  isForbiddenRead,
  readFailureExplanation,
  readFailureServerDetail,
} from '@/lib/read-failure'

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
  retrying,
  embedded = false,
}: {
  resource: string
  error: unknown
  cached?: boolean
  onRetry: () => void
  retrying: boolean
  embedded?: boolean
}) {
  const forbidden = isForbiddenRead(error)
  const serverDetail = readFailureServerDetail(error)
  const Icon = forbidden ? ShieldAlert : AlertTriangle
  return (
    <Alert
      variant="warning"
      data-read-failure={forbidden ? 'forbidden' : 'failed'}
      className={
        embedded ? 'rounded-none border-0 bg-transparent p-0' : undefined
      }
    >
      {!embedded && <Icon className="size-4" />}
      <AlertTitle>
        {forbidden ? `${resource}: access denied` : `${resource} unavailable`}
      </AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{readFailureExplanation(error)}</p>
        {serverDetail && (
          <p className="break-words">
            <span className="font-medium">Server response:</span>{' '}
            {serverDetail}
          </p>
        )}
        {cached && (
          <p>Showing last-known data. It is stale and may have changed.</p>
        )}
        <Button
          variant="outline"
          size="sm"
          aria-disabled={retrying}
          onClick={() => {
            if (!retrying) onRetry()
          }}
        >
          {retrying ? 'Retrying…' : 'Retry'}
        </Button>
      </AlertDescription>
    </Alert>
  )
}
