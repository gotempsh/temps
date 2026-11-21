// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pieces of the OpenTelemetry setup wizard for apps Temps does not run: the
 * project-wide note and the `.env` block, which loads its URL from
 * `GET /projects/{id}/ingest-setup`.
 */
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'

import { Skeleton } from '@/components/ui/skeleton'
import { externalOtelEnvBlock } from '@/lib/plain-http-setup'

import { findSignal, useIngestSetup } from './use-ingest-setup'

/**
 * OTLP data from an app Temps does not run is stored for the whole project:
 * the environment filter finds environments through deployments, and these
 * exports carry none. Say so instead of offering an environment picker that
 * would change nothing.
 */
export function ProjectWideOtlpNote() {
  return (
    <p className="text-xs text-muted-foreground">
      Traces and logs from apps outside Temps belong to the whole project and
      show under All environments.
    </p>
  )
}

/** The OTel SDK `.env` for an app hosted elsewhere, with load and error states. */
export function ExternalOtelEnvBlock({
  projectId,
  serviceName,
}: {
  projectId: number
  serviceName: string
}) {
  const query = useIngestSetup(projectId)
  const traces = findSignal(query.data, 'traces')

  if (traces) {
    // A failed background refresh keeps the variables that already loaded.
    return (
      <div className="space-y-3">
        {query.isError && (
          <Alert>
            <AlertTitle>Could not refresh the OTLP endpoint</AlertTitle>
            <AlertDescription className="flex flex-wrap items-center gap-2">
              <span>Showing the last loaded values.</span>
              <Button
                size="sm"
                variant="outline"
                disabled={query.isFetching}
                onClick={() => void query.refetch()}
              >
                Retry
              </Button>
            </AlertDescription>
          </Alert>
        )}
        <CodeBlock
          defaultWrap
          code={externalOtelEnvBlock(traces, serviceName)}
          language="bash"
          title=".env"
        />
      </div>
    )
  }
  if (query.isError) {
    return (
      <Alert variant="destructive">
        <AlertTitle>Could not load the OTLP endpoint</AlertTitle>
        <AlertDescription className="flex flex-wrap items-center gap-2">
          <span>
            {query.error?.detail ?? 'The ingest setup request failed.'}
          </span>
          <Button
            size="sm"
            variant="outline"
            disabled={query.isFetching}
            onClick={() => void query.refetch()}
          >
            Retry
          </Button>
        </AlertDescription>
      </Alert>
    )
  }
  return <Skeleton className="h-28 w-full rounded-2xl" />
}
