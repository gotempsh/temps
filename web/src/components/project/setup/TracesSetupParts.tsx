// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Pieces of the OpenTelemetry setup wizard: the environment picker and the
 * `.env` block for apps Temps does not run, which loads its URL from
 * `GET /projects/{id}/ingest-setup`.
 */
import type { EnvironmentResponse } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { CodeBlock } from '@/components/ui/code-block'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { externalOtelEnvBlock } from '@/lib/plain-http-setup'

import { findSignal, useIngestSetup } from './use-ingest-setup'

/**
 * "Environment:" and its select. The trigger is named by the visible label
 * and its own value, so screen readers hear "Environment: production".
 */
export function SetupEnvironmentSelect({
  id,
  environments,
  value,
  onValueChange,
}: {
  id: string
  environments: EnvironmentResponse[] | undefined
  value: string
  onValueChange: (value: string) => void
}) {
  const labelId = `${id}-label`
  return (
    <div className="flex items-center gap-3">
      <span id={labelId} className="shrink-0 text-xs text-muted-foreground">
        Environment:
      </span>
      <Select value={value} onValueChange={onValueChange}>
        <SelectTrigger
          id={id}
          aria-labelledby={`${labelId} ${id}`}
          className="h-8 w-full sm:w-[200px]"
        >
          <SelectValue placeholder="Select environment" />
        </SelectTrigger>
        <SelectContent>
          {environments?.map((env) => (
            <SelectItem key={env.id} value={String(env.id)}>
              {env.name}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  )
}

/** The OTel SDK `.env` for an app hosted elsewhere, with load and error states. */
export function ExternalOtelEnvBlock({
  projectId,
  environmentId,
  serviceName,
}: {
  projectId: number
  environmentId?: number
  serviceName: string
}) {
  const query = useIngestSetup(projectId, environmentId)
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
