// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Alert, AlertDescription, AlertTitle } from './alert'
import { Button } from './button'
import { readFailureExplanation } from '@/lib/read-failure'

/** Inline read recovery. Retry only replays a query, never a mutation. */
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
  return (
    <Alert
      variant="warning"
      className={
        embedded ? 'rounded-none border-0 bg-transparent p-0' : undefined
      }
    >
      <AlertTitle>{resource} unavailable</AlertTitle>
      <AlertDescription className="space-y-2">
        <p>{readFailureExplanation(error)}</p>
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
