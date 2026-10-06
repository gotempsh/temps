// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Presentational states for the Trigger Backup dialog's destination list:
 * loading, failed read, failed refresh over cached data, and a genuinely
 * empty list. Kept separate from the dialog (which renders inside a Radix
 * portal) so each state can be rendered and asserted on its own.
 */

import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { EmptyState } from '@/components/ui/empty-state'
import { Skeleton } from '@/components/ui/skeleton'
import { AlertCircle, AlertTriangle, HardDrive, RefreshCw } from 'lucide-react'
import { Link } from 'react-router'
import {
  DESTINATIONS_LOAD_FAILED_MESSAGE,
  DESTINATIONS_REFRESH_FAILED_MESSAGE,
  type DestinationFailure,
} from './trigger-backup-state'

/** Console route that creates an S3 source. */
export const CREATE_S3_SOURCE_PATH = '/backups/s3-sources/new'

/** Placeholder shaped like the destination and backup-type fields. */
export function DestinationsSkeleton() {
  return (
    <div
      className="space-y-4"
      aria-busy="true"
      aria-label="Loading backup destinations"
    >
      <div className="space-y-2">
        <Skeleton className="h-4 w-36" />
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-3 w-3/4" />
      </div>
      <div className="space-y-2">
        <Skeleton className="h-4 w-24" />
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-3 w-2/3" />
      </div>
    </div>
  )
}

interface RetryProps {
  failure: DestinationFailure
  onRetry: () => void
  /** True while the retry triggered by `onRetry` is in flight. */
  isRetrying: boolean
}

function FailureDetails({ failure }: { failure: DestinationFailure }) {
  return (
    <>
      {failure.explanation ? <p>{failure.explanation}</p> : null}
      {failure.detail ? (
        <p className="text-muted-foreground">
          Server response: {failure.detail}
        </p>
      ) : null}
    </>
  )
}

function RetryButton({
  onRetry,
  isRetrying,
}: Pick<RetryProps, 'onRetry' | 'isRetrying'>) {
  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      onClick={onRetry}
      disabled={isRetrying}
    >
      <RefreshCw
        className={isRetrying ? 'mr-2 h-4 w-4 animate-spin' : 'mr-2 h-4 w-4'}
      />
      {isRetrying ? 'Retrying...' : 'Retry'}
    </Button>
  )
}

/**
 * The list could not be read and nothing is cached. Deliberately offers no
 * "create a destination" link: the destinations may well exist.
 */
export function DestinationsLoadError({
  failure,
  onRetry,
  isRetrying,
}: RetryProps) {
  return (
    <Alert variant="destructive">
      <AlertCircle className="h-4 w-4" />
      <AlertTitle className="leading-snug">
        {DESTINATIONS_LOAD_FAILED_MESSAGE}
      </AlertTitle>
      <AlertDescription className="space-y-3">
        <FailureDetails failure={failure} />
        <RetryButton onRetry={onRetry} isRetrying={isRetrying} />
      </AlertDescription>
    </Alert>
  )
}

/**
 * A refresh failed but the last loaded list is still shown. Non-blocking:
 * the form and the user's selection stay usable underneath it.
 */
export function DestinationsRefreshWarning({
  failure,
  onRetry,
  isRetrying,
}: RetryProps) {
  return (
    <Alert variant="warning">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle className="leading-snug">
        {DESTINATIONS_REFRESH_FAILED_MESSAGE}
      </AlertTitle>
      <AlertDescription className="space-y-3">
        <FailureDetails failure={failure} />
        <RetryButton onRetry={onRetry} isRetrying={isRetrying} />
      </AlertDescription>
    </Alert>
  )
}

/** The server successfully reported that no destination exists. */
export function NoDestinationsConfigured() {
  return (
    <div className="rounded-lg border bg-card text-card-foreground">
      <EmptyState
        size="compact"
        icon={HardDrive}
        title="No S3 sources configured"
        description="You need to create an S3 source before you can trigger backups."
        action={
          <Button asChild>
            <Link to={CREATE_S3_SOURCE_PATH}>Create S3 source</Link>
          </Button>
        }
      />
    </div>
  )
}
