// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { RestoreRunView } from '@/api/client/types.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { CopyButton } from '@/components/ui/copy-button'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import {
  AlertCircle,
  AlertTriangle,
  ArrowLeft,
  CheckCircle2,
  CircleDashed,
  Clock,
  Database,
  Info,
  Loader2,
  RefreshCw,
  RotateCcw,
  XCircle,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import {
  PHASES,
  interruptedFallbackMessage,
  phaseStates,
  runTrackingProblemCopy,
  type AttachReason,
  type PhaseState,
  type RunTrackingView,
  type TimeFormatter,
  formatConfirmedTime,
} from './restore-state'

export interface RunTrackingPanelProps {
  serviceId: number
  runId: number | 'invalid'
  view: RunTrackingView
  attachReason?: AttachReason
  /** Re-read the run status. Never re-runs the restore itself. */
  onRetryStatus: () => void
  retrying: boolean
  onBack: () => void
  onStartNew: () => void
  onOpenRestored: (serviceId: number) => void
  formatTime?: TimeFormatter
}

export function RunTrackingPanel({
  serviceId,
  runId,
  view,
  attachReason,
  onRetryStatus,
  retrying,
  onBack,
  onStartNew,
  onOpenRestored,
  formatTime = formatConfirmedTime,
}: RunTrackingPanelProps) {
  const [checkedAfterInterrupt, setCheckedAfterInterrupt] = useState(false)
  const run = runFromView(view)
  const terminal = view.kind === 'terminal'

  return (
    <div className="space-y-4">
      {attachReason && typeof runId === 'number' ? (
        <Alert>
          <Info className="h-4 w-4" />
          <AlertDescription>
            {attachReason === 'already_active'
              ? `A restore was already running on this database, so a new one was not started. Following run #${runId} instead.`
              : `Following restore run #${runId}, which was already in progress for this database.`}
          </AlertDescription>
        </Alert>
      ) : null}

      <Card className="shadow-none">
        <CardHeader>
          <div className="flex flex-wrap items-center gap-2">
            <RunStatusBadge view={view} />
            {run ? (
              <span className="text-sm text-muted-foreground">
                mode <code>{run.mode}</code>
              </span>
            ) : null}
            {typeof runId === 'number' ? (
              <span className="inline-flex items-center gap-1 text-sm text-muted-foreground">
                run #{runId}
                <CopyButton
                  value={String(runId)}
                  label="Copy run ID"
                  className="h-7 w-7 rounded-md"
                />
              </span>
            ) : null}
          </div>
        </CardHeader>
        <CardContent className="space-y-4">
          <RunStatusProblem
            view={view}
            onRetryStatus={onRetryStatus}
            retrying={retrying}
            formatTime={formatTime}
          />
          <PhaseList view={view} />
          <RunOutcome view={view} />
        </CardContent>
      </Card>

      {view.kind === 'terminal' && view.outcome === 'interrupted' ? (
        <div className="space-y-3 rounded-lg border p-4">
          <p className="text-sm font-medium">Before you restore again</p>
          <p className="text-sm text-muted-foreground">
            Check that the database is healthy and holds the data you expect. A
            partial restore may have left it inconsistent.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" size="sm" asChild>
              <Link to={`/storage/${serviceId}`}>Check database health</Link>
            </Button>
            <Button variant="outline" size="sm" asChild>
              <Link to={`/storage/${serviceId}/logs`}>View database logs</Link>
            </Button>
          </div>
          <div className="flex items-center gap-2">
            <Checkbox
              id="interrupted-checked"
              checked={checkedAfterInterrupt}
              onCheckedChange={(value) =>
                setCheckedAfterInterrupt(value === true)
              }
            />
            <Label htmlFor="interrupted-checked" className="cursor-pointer">
              I checked the database&apos;s health and data
            </Label>
          </div>
        </div>
      ) : null}

      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" onClick={onBack}>
            <ArrowLeft className="h-4 w-4 mr-2" />
            Back to service
          </Button>
          {view.kind === 'terminal' &&
          view.outcome === 'completed' &&
          view.run.target_service_id != null ? (
            <Button onClick={() => onOpenRestored(view.run.target_service_id!)}>
              <Database className="h-4 w-4 mr-2" />
              Open restored service
            </Button>
          ) : null}
          {terminal ? (
            <Button
              variant="outline"
              onClick={onStartNew}
              disabled={
                view.kind === 'terminal' &&
                view.outcome === 'interrupted' &&
                !checkedAfterInterrupt
              }
            >
              <RotateCcw className="h-4 w-4 mr-2" />
              Start a new restore
            </Button>
          ) : null}
          {view.kind === 'not_found' ? (
            <Button variant="outline" onClick={onStartNew}>
              <RotateCcw className="h-4 w-4 mr-2" />
              Back to restore setup
            </Button>
          ) : null}
        </div>
        {!terminal && view.kind !== 'not_found' ? (
          <p className="text-sm text-muted-foreground">
            Leaving this page does not stop the restore: it keeps running on the
            server. Come back to this page&apos;s address to keep following it.
          </p>
        ) : null}
      </div>
    </div>
  )
}

function runFromView(view: RunTrackingView): RestoreRunView | undefined {
  switch (view.kind) {
    case 'tracking':
    case 'terminal':
      return view.run
    case 'stale':
    case 'forbidden':
    case 'not_found':
      return view.lastRun
    case 'attaching':
      return undefined
  }
}

function RunStatusBadge({ view }: { view: RunTrackingView }) {
  switch (view.kind) {
    case 'attaching':
      return <Badge variant="secondary">loading status</Badge>
    case 'tracking':
      return <Badge variant="secondary">{view.run.status}</Badge>
    case 'terminal':
      return (
        <Badge
          variant={
            view.outcome === 'completed'
              ? 'success'
              : view.outcome === 'failed'
                ? 'destructive'
                : view.outcome === 'interrupted'
                  ? 'warning'
                  : 'secondary'
          }
        >
          {view.outcome}
        </Badge>
      )
    case 'stale':
    case 'forbidden':
      return <Badge variant="outline">status unknown</Badge>
    case 'not_found':
      return <Badge variant="outline">not found</Badge>
  }
}

function RunStatusProblem({
  view,
  onRetryStatus,
  retrying,
  formatTime,
}: {
  view: RunTrackingView
  onRetryStatus: () => void
  retrying: boolean
  formatTime: TimeFormatter
}) {
  if (
    view.kind !== 'stale' &&
    view.kind !== 'forbidden' &&
    view.kind !== 'not_found'
  )
    return null
  const copy = runTrackingProblemCopy(view, formatTime)
  return (
    <Alert variant={view.kind === 'not_found' ? 'default' : 'warning'}>
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>{copy.title}</AlertTitle>
      <AlertDescription className="space-y-3">
        <p>{copy.description}</p>
        {view.kind !== 'not_found' ? (
          <Button
            variant="outline"
            size="sm"
            onClick={onRetryStatus}
            disabled={retrying}
          >
            {retrying ? (
              <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            ) : (
              <RefreshCw className="h-4 w-4 mr-2" />
            )}
            Retry
          </Button>
        ) : null}
      </AlertDescription>
    </Alert>
  )
}

function PhaseList({ view }: { view: RunTrackingView }) {
  if (view.kind === 'attaching') {
    return (
      <ol className="space-y-3" aria-label="Loading restore status">
        {PHASES.map((p) => (
          <li key={p.id} className="flex items-center gap-3">
            <Skeleton className="h-5 w-5 rounded-full" />
            <Skeleton className="h-4 w-32" />
          </li>
        ))}
      </ol>
    )
  }
  const run = runFromView(view)
  if (!run) return null
  const live = view.kind === 'tracking' || view.kind === 'terminal'
  return (
    <ol className="space-y-3">
      {phaseStates(run, live).map((p) => (
        <li key={p.id} className="flex items-center gap-3 text-sm">
          <PhaseIcon state={p.state} />
          <span className={phaseLabelClass(p.state)}>
            {p.label}
            {p.state === 'last_known' ? (
              <span className="ml-2 text-xs text-muted-foreground">
                (last confirmed)
              </span>
            ) : null}
          </span>
        </li>
      ))}
    </ol>
  )
}

function PhaseIcon({ state }: { state: PhaseState }) {
  switch (state) {
    case 'done':
      return <CheckCircle2 className="h-5 w-5 text-success" />
    case 'active':
      return <Loader2 className="h-5 w-5 animate-spin text-primary" />
    case 'last_known':
      return <CircleDashed className="h-5 w-5 text-muted-foreground" />
    case 'failed':
      return <XCircle className="h-5 w-5 text-destructive" />
    case 'interrupted':
      return <AlertTriangle className="h-5 w-5 text-warning" />
    case 'stopped':
      return <XCircle className="h-5 w-5 text-muted-foreground" />
    case 'pending':
      return <Clock className="h-5 w-5 text-muted-foreground" />
  }
}

function phaseLabelClass(state: PhaseState): string {
  switch (state) {
    case 'pending':
    case 'stopped':
      return 'text-muted-foreground'
    case 'failed':
      return 'text-destructive'
    case 'interrupted':
      return 'text-warning'
    default:
      return ''
  }
}

function RunOutcome({ view }: { view: RunTrackingView }) {
  if (view.kind !== 'terminal') return null
  const { run } = view
  switch (view.outcome) {
    case 'completed':
      return null
    case 'failed':
      return (
        <Alert variant="destructive">
          <AlertCircle className="h-4 w-4" />
          <AlertTitle>Restore failed</AlertTitle>
          <AlertDescription className="break-words">
            {run.error_message ??
              `Restore run ${run.id} failed without an error message.`}
          </AlertDescription>
        </Alert>
      )
    case 'cancelled':
      return (
        <Alert>
          <Info className="h-4 w-4" />
          <AlertTitle>Restore cancelled</AlertTitle>
          <AlertDescription className="break-words">
            {run.error_message ?? `Restore run ${run.id} was cancelled.`}
          </AlertDescription>
        </Alert>
      )
    case 'interrupted':
      return (
        <Alert variant="warning">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle>Restore interrupted</AlertTitle>
          <AlertDescription className="break-words">
            {run.error_message ?? interruptedFallbackMessage(run.phase)}
          </AlertDescription>
        </Alert>
      )
  }
}
