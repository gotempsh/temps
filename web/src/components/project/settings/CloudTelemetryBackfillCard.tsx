// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import { getCurrentBulkActivationJobOptions } from '@/api/client/@tanstack/react-query.gen'
import type { BulkActivationJobProjectResponse } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
import { Progress } from '@/components/ui/progress'
import { Skeleton } from '@/components/ui/skeleton'
import { getCloudBackfillStatusOptions } from '@/api/client/@tanstack/react-query.gen'
import type { CloudBackfillStatusResponse } from '@/api/client/types.gen'
import { PROJECT_STATUS_HINTS } from '@/lib/cloud-telemetry-activation'
import { isBackfillStalled } from '@/lib/backfill-stall'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, ArrowRight, Cloud, Layers } from 'lucide-react'
import { Link } from 'react-router'

/** Where the instance-wide activation section lives (ADR-042 §11). */
const ACTIVATION_SECTION_PATH =
  '/settings/otel-pipeline#cloud-telemetry-activation'

/**
 * Progress of the Temps Cloud telemetry backfill for this project (ADR-040 §1).
 *
 * The backfill deliberately runs as `temps backfill cloud-telemetry`, out of
 * process — it must not contend with live ingest, and paying to send historical
 * data should be a deliberate operator act rather than a button someone clicks
 * by accident. That is exactly why this card exists: "the Console cannot
 * trigger it" must never become "the Console cannot see it".
 *
 * So the card is always rendered, in every state, and never disappears:
 *
 * - **Not opted in** — says what is missing and links to the settings page that
 *   raises fidelity, instead of silently showing nothing.
 * - **Never run** — shows the exact command, copyable. An operator who has
 *   never heard of the backfill learns it exists from here.
 * - **Running** — spans processed / total / percent, plus a stalled warning if
 *   the driving process stopped writing progress, rather than a bar that spins
 *   forever.
 * - **Completed / failed** — when, or why, verbatim.
 */
export function CloudTelemetryBackfillCard({
  project,
}: {
  project: ProjectResponse
}) {
  const { data, isPending, isError, error, refetch } = useQuery(
    getCloudBackfillStatusOptions({ path: { project_id: project.id } })
  )

  // A running backfill this project's own settings page never started is the
  // confusing case, so that is the only case worth asking about. The per-project
  // status carries no `bulk_job_id`, so membership is derived from the
  // instance-wide job's project list. The endpoint is instance-admin only: a
  // `403` simply leaves the note off, which is the same view as before.
  const { data: bulkJob } = useQuery({
    ...getCurrentBulkActivationJobOptions(),
    enabled: data?.status === 'running',
    retry: false,
    refetchInterval: 30_000,
  })

  const bulkJobProject = bulkJob?.projects.find(
    (row) => row.project_id === project?.id
  )

  return (
    <Card className="bg-background text-foreground">
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Cloud className="h-4 w-4" aria-hidden="true" />
          Temps Cloud telemetry backfill
        </CardTitle>
        <CardDescription>
          Raising this project&apos;s telemetry fidelity only affects spans
          received afterwards. The backfill sends the history you already have
          so Cloud can serve it back.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <BackfillBody
          isPending={isPending}
          isError={isError}
          error={error}
          data={data}
          onRetry={() => void refetch()}
          bulkJobId={bulkJobProject ? (bulkJob?.batch_id ?? null) : null}
          bulkJobProject={bulkJobProject}
        />
      </CardContent>
    </Card>
  )
}

function BackfillBody({
  isPending,
  isError,
  error,
  data,
  onRetry,
  bulkJobId,
  bulkJobProject,
}: {
  isPending: boolean
  isError: boolean
  error: unknown
  data: CloudBackfillStatusResponse | undefined
  onRetry: () => void
  /** The bulk activation that owns this backfill, when one does. */
  bulkJobId: string | null
  bulkJobProject: BulkActivationJobProjectResponse | undefined
}) {
  if (isPending) {
    // Skeletons that match the real layout, so the card does not collapse and
    // re-expand when the status arrives.
    return (
      <div className="space-y-3">
        <Skeleton className="h-5 w-40" />
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-9 w-full" />
      </div>
    )
  }

  if (isError || !data) {
    // A failure to read the status is itself a state worth showing. Silence
    // here would look identical to "no backfill has ever run".
    return (
      <Alert variant="destructive">
        <AlertCircle className="h-4 w-4" aria-hidden="true" />
        <AlertTitle>Could not read the backfill status</AlertTitle>
        <AlertDescription className="space-y-3">
          <p>{errorMessage(error)}</p>
          <Button variant="outline" size="sm" onClick={onRetry}>
            Try again
          </Button>
        </AlertDescription>
      </Alert>
    )
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <StatusBadge status={data} />
        <span className="text-sm text-muted-foreground">
          Fidelity: <code className="font-mono">{data.fidelity}</code>
        </span>
      </div>

      {bulkJobId && bulkJobProject && (
        <PartOfBulkActivation batchId={bulkJobId} row={bulkJobProject} />
      )}

      {!data.backfill_available && <NotOptedIn status={data} />}
      {data.status === 'running' && <RunningProgress status={data} />}
      {data.status === 'completed' && <Completed status={data} />}
      {data.status === 'failed' && <Failed status={data} />}

      <CommandBlock status={data} ownedByBulkJob={!!bulkJobProject} />
    </div>
  )
}

/**
 * Says out loud that this run belongs to an instance-wide activation.
 *
 * Without it, a project whose backfill someone else queued reads as
 * "already running" with no explanation, and the obvious next move — running
 * the CLI command below — would contend with the job that is already doing it.
 */
function PartOfBulkActivation({
  batchId,
  row,
}: {
  batchId: string
  row: BulkActivationJobProjectResponse
}) {
  return (
    <Alert>
      <Layers className="h-4 w-4" aria-hidden="true" />
      <AlertTitle>Part of a bulk Cloud activation</AlertTitle>
      <AlertDescription className="space-y-3">
        <p>
          This backfill was not started for this project on its own — it is one
          step of an instance-wide activation ({row.status}).{' '}
          {PROJECT_STATUS_HINTS[row.status]}
        </p>
        <p className="font-mono text-xs break-all text-muted-foreground">
          Activation {batchId}
        </p>
        <Button asChild variant="outline" size="sm" className="gap-1.5">
          <Link to={ACTIVATION_SECTION_PATH}>
            See the whole activation
            <ArrowRight className="size-3.5" />
          </Link>
        </Button>
      </AlertDescription>
    </Alert>
  )
}

function StatusBadge({ status }: { status: CloudBackfillStatusResponse }) {
  if (isBackfillStalled(status)) {
    return <Badge variant="destructive">Stalled</Badge>
  }
  switch (status.status) {
    case 'running':
      return <Badge variant="secondary">Running</Badge>
    case 'completed':
      return <Badge variant="outline">Completed</Badge>
    case 'failed':
      return <Badge variant="destructive">Failed</Badge>
    case 'not_started':
    default:
      return <Badge variant="outline">Not started</Badge>
  }
}

/**
 * The onboarding state. Never renders nothing: it states exactly what is
 * missing and links straight to the page that fixes it.
 */
function NotOptedIn({ status }: { status: CloudBackfillStatusResponse }) {
  return (
    <Alert>
      <AlertCircle className="h-4 w-4" aria-hidden="true" />
      <AlertTitle>This project is not set up for Cloud read-back</AlertTitle>
      <AlertDescription className="space-y-3">
        <p>
          Telemetry fidelity is <code className="font-mono">metered</code>, so
          only pseudonymised identifiers and a placeholder span name leave this
          instance — enough to bill and to prove the instance is alive, but not
          enough to read back. A backfill would be refused. Raise fidelity to{' '}
          <code className="font-mono">queryable</code> first.
        </p>
        {status.setup_path && (
          <Button asChild variant="outline" size="sm">
            <Link to={status.setup_path}>Open Cloud settings</Link>
          </Button>
        )}
      </AlertDescription>
    </Alert>
  )
}

function RunningProgress({ status }: { status: CloudBackfillStatusResponse }) {
  const stalled = isBackfillStalled(status)
  return (
    <div className="space-y-3">
      <Progress value={status.percent_complete ?? 0} />
      <div className="flex flex-col gap-1 text-sm text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
        <span>
          {status.spans_processed.toLocaleString()} of{' '}
          {status.spans_total.toLocaleString()} spans
          {typeof status.percent_complete === 'number' &&
            ` (${status.percent_complete.toFixed(1)}%)`}
        </span>
        {status.started_at && (
          <span>Started {new Date(status.started_at).toLocaleString()}</span>
        )}
      </div>
      {status.window_from && status.window_to && (
        <p className="text-sm text-muted-foreground">
          Filling {new Date(status.window_from).toLocaleString()} →{' '}
          {new Date(status.window_to).toLocaleString()}
        </p>
      )}
      {stalled && (
        // A run whose process died would otherwise leave a bar that never
        // moves and never explains itself.
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" aria-hidden="true" />
          <AlertTitle>This backfill has stopped reporting progress</AlertTitle>
          <AlertDescription>
            The last update was{' '}
            {status.updated_at
              ? new Date(status.updated_at).toLocaleString()
              : 'a while ago'}
            . The process running it has probably exited. Re-running the command
            below is safe — the backfill is resumable and Temps Cloud discards
            duplicates.
          </AlertDescription>
        </Alert>
      )}
    </div>
  )
}

function Completed({ status }: { status: CloudBackfillStatusResponse }) {
  return (
    <p className="text-sm text-muted-foreground">
      {status.spans_processed.toLocaleString()} span
      {status.spans_processed === 1 ? '' : 's'} sent
      {status.completed_at &&
        `, finished ${new Date(status.completed_at).toLocaleString()}`}
      .
    </p>
  )
}

function Failed({ status }: { status: CloudBackfillStatusResponse }) {
  return (
    <Alert variant="destructive">
      <AlertCircle className="h-4 w-4" aria-hidden="true" />
      <AlertTitle>The last backfill stopped early</AlertTitle>
      <AlertDescription className="space-y-2">
        {/* Verbatim: the person reading this is not the one who saw the
            terminal output, and a paraphrase loses the actionable part. */}
        <p className="font-mono text-xs break-words">
          {status.last_error ?? 'No reason was recorded.'}
        </p>
        <p>
          {status.spans_processed.toLocaleString()} of{' '}
          {status.spans_total.toLocaleString()} spans were sent. Re-running is
          safe: the backfill resumes and Temps Cloud discards duplicates.
        </p>
      </AlertDescription>
    </Alert>
  )
}

/**
 * The command, in every state. This is the only place an operator can discover
 * that the capability exists at all, so it is never hidden behind a state.
 */
function CommandBlock({
  status,
  ownedByBulkJob,
}: {
  status: CloudBackfillStatusResponse
  ownedByBulkJob: boolean
}) {
  return (
    <div className="space-y-2">
      <p className="text-sm font-medium">
        {ownedByBulkJob
          ? 'Running this while the activation is in flight would duplicate work'
          : status.status === 'not_started'
            ? 'Run this on the instance to start a backfill'
            : 'Run this on the instance to start another backfill'}
      </p>
      <div className="flex items-start gap-2 rounded-lg border bg-muted/40 p-3">
        <code className="min-w-0 flex-1 font-mono text-xs break-all">
          {status.command}
        </code>
        <CopyButton value={status.command} minimal label="Copy command" />
      </div>
      <p className="text-sm text-muted-foreground">
        <code className="font-mono">--dry-run</code> reports the row count, the
        estimated metered bytes and one example record without sending anything.
        Drop it to run for real. Stop{' '}
        <code className="font-mono">temps serve</code> first — the backfill
        drives the same Cloud link the live mirror uses.
      </p>
    </div>
  )
}

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message) return error.message
  if (typeof error === 'string' && error) return error
  return 'The instance did not return a status for this project.'
}
