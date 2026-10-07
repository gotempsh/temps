// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Presentational content of the header operations tray. Holds no data
 * fetching so every state (loading, error, empty, populated, local entries)
 * renders deterministically from props.
 */
import type { OperationEntry } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Separator } from '@/components/ui/separator'
import { Skeleton } from '@/components/ui/skeleton'
import {
  formatRelativeShort,
  LOCAL_OPERATION_ICON,
  OPERATION_KIND_ICON,
  OPERATION_KIND_LABEL,
  OPERATION_STATUS_LABEL,
  operationContext,
  operationStatusVariant,
  operationTimestamp,
} from '@/lib/operations'
import { cn } from '@/lib/utils'
import { AlertCircle, ChevronRight } from 'lucide-react'
import { Link } from 'react-router'
import type { LocalOperation } from './operations-tray-store'

export const OPERATIONS_EMPTY_MESSAGE =
  'Nothing running. Deployments, rollbacks, restores, backups and autofix runs you start appear here.'

/** One independently fetched, independently paged section of the tray. */
export interface OperationsSectionState {
  operations: readonly OperationEntry[]
  isPending: boolean
  isError: boolean
  errorMessage?: string | null
  onRetry: () => void
  /** More pages exist on the server. */
  hasMore: boolean
  isFetchingMore: boolean
  onLoadMore: () => void
}

export interface RunningSectionState extends OperationsSectionState {
  /** Counted in-flight operations not loaded as rows yet. */
  notLoaded: number
}

export interface OperationsTrayPanelProps {
  /** In-flight work (`status=running`); the badge counts exactly these. */
  running: RunningSectionState
  /** Finished history (`status=finished`), newest first. */
  recent: OperationsSectionState
  localOperations: readonly LocalOperation[]
  runningCount: number
  /** Called when a row is followed, so the popover can close. */
  onNavigate: () => void
  /** Epoch milliseconds used for relative times. */
  now: number
}

export function OperationsTrayPanel({
  running,
  recent,
  localOperations,
  runningCount,
  onNavigate,
  now,
}: OperationsTrayPanelProps) {
  const inFlight = runningCount + localOperations.length
  return (
    <div>
      <div className="flex items-center justify-between px-3 py-2.5">
        <p className="text-sm font-medium">Operations</p>
        {inFlight > 0 && (
          <p className="text-xs text-muted-foreground tabular-nums">
            {inFlight} running
          </p>
        )}
      </div>
      <Separator />
      <div className="max-h-[420px] overflow-y-auto">
        {localOperations.length > 0 && (
          <ul role="list" className="divide-y divide-border border-b">
            {localOperations.map((operation) => (
              <li key={operation.id}>
                <LocalOperationRow operation={operation} now={now} />
              </li>
            ))}
          </ul>
        )}
        <OperationsTrayBody
          running={running}
          recent={recent}
          hasLocal={localOperations.length > 0}
          onNavigate={onNavigate}
          now={now}
        />
      </div>
    </div>
  )
}

function OperationsTrayBody({
  running,
  recent,
  hasLocal,
  onNavigate,
  now,
}: {
  running: RunningSectionState
  recent: OperationsSectionState
  hasLocal: boolean
  onNavigate: () => void
  now: number
}) {
  if (running.isPending && recent.isPending) return <OperationsTraySkeleton />
  if (running.isError && recent.isError) {
    return (
      <OperationsTrayError
        title="Couldn't load operations"
        message={running.errorMessage ?? recent.errorMessage}
        onRetry={() => {
          running.onRetry()
          recent.onRetry()
        }}
      />
    )
  }
  const settledEmpty =
    !running.isPending &&
    !recent.isPending &&
    !running.isError &&
    !recent.isError &&
    running.operations.length === 0 &&
    recent.operations.length === 0
  if (settledEmpty) return hasLocal ? null : <OperationsTrayEmpty />
  return (
    <>
      <OperationSection
        label="Running"
        errorTitle="Couldn't load running operations"
        loadMoreLabel={runningLoadMoreLabel(running.notLoaded)}
        state={running}
        onNavigate={onNavigate}
        now={now}
      />
      <OperationSection
        label="Recent"
        errorTitle="Couldn't load recent operations"
        loadMoreLabel="Load older operations"
        state={recent}
        onNavigate={onNavigate}
        now={now}
      />
    </>
  )
}

function runningLoadMoreLabel(notLoaded: number): string {
  return notLoaded > 0 ? `Show ${notLoaded} more running` : 'Show more running'
}

function OperationSection({
  label,
  errorTitle,
  loadMoreLabel,
  state,
  onNavigate,
  now,
}: {
  label: string
  errorTitle: string
  loadMoreLabel: string
  state: OperationsSectionState
  onNavigate: () => void
  now: number
}) {
  if (state.isPending) {
    return <OperationsTraySkeleton rows={1} label={`Loading ${label}`} />
  }
  if (state.isError) {
    return (
      <OperationsTrayError
        title={errorTitle}
        message={state.errorMessage}
        onRetry={state.onRetry}
      />
    )
  }
  if (state.operations.length === 0) return null
  return (
    <section aria-label={label} className="border-b last:border-b-0">
      <p className="px-3 pb-1 pt-2.5 text-xs font-medium text-muted-foreground">
        {label}
      </p>
      <ul role="list" className="divide-y divide-border">
        {state.operations.map((operation) => (
          <li key={operation.id}>
            <OperationRow
              operation={operation}
              onNavigate={onNavigate}
              now={now}
            />
          </li>
        ))}
      </ul>
      {state.hasMore && (
        <div className="border-t px-3 py-2">
          <Button
            variant="ghost"
            size="sm"
            className="w-full"
            disabled={state.isFetchingMore}
            onClick={state.onLoadMore}
          >
            {state.isFetchingMore ? 'Loading…' : loadMoreLabel}
          </Button>
        </div>
      )}
    </section>
  )
}

function OperationRow({
  operation,
  onNavigate,
  now,
}: {
  operation: OperationEntry
  onNavigate: () => void
  now: number
}) {
  const Icon = OPERATION_KIND_ICON[operation.kind]
  const context = operationContext(operation)
  const showReason =
    (operation.status === 'failed' || operation.status === 'cancelled') &&
    !!operation.failure_reason
  return (
    <Link
      to={operation.link}
      onClick={onNavigate}
      className="flex items-start gap-3 px-3 py-3 transition-colors hover:bg-accent focus-visible:bg-accent focus-visible:outline-none"
    >
      <span
        className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"
        aria-hidden="true"
      >
        <Icon className="size-3.5" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline justify-between gap-2">
          <p className="truncate text-sm font-medium">
            <span className="sr-only">
              {OPERATION_KIND_LABEL[operation.kind]}:{' '}
            </span>
            {operation.title}
          </p>
          <p className="shrink-0 text-xs text-muted-foreground tabular-nums">
            {formatRelativeShort(operationTimestamp(operation), now)}
          </p>
        </div>
        <div className="mt-1 flex min-w-0 items-center gap-2">
          <Badge
            variant={operationStatusVariant(operation.status)}
            className="shrink-0 px-1.5 py-0 text-[11px] font-medium"
          >
            {OPERATION_STATUS_LABEL[operation.status]}
          </Badge>
          {context && (
            <span className="truncate text-xs text-muted-foreground">
              {context}
            </span>
          )}
        </div>
        {showReason && (
          <p
            className={cn(
              'mt-1 line-clamp-2 text-xs',
              // A cancellation is the operator's choice, not an error.
              operation.status === 'failed'
                ? 'text-destructive'
                : 'text-muted-foreground'
            )}
          >
            {operation.failure_reason}
          </p>
        )}
      </div>
      <ChevronRight
        className="mt-1 size-4 shrink-0 text-muted-foreground"
        aria-hidden="true"
      />
    </Link>
  )
}

function LocalOperationRow({
  operation,
  now,
}: {
  operation: LocalOperation
  now: number
}) {
  const Icon = LOCAL_OPERATION_ICON
  return (
    <div className="flex items-start gap-3 px-3 py-3">
      <span
        className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"
        aria-hidden="true"
      >
        <Icon className="size-3.5 animate-spin" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline justify-between gap-2">
          <p className="truncate text-sm font-medium">{operation.title}</p>
          <p className="shrink-0 text-xs text-muted-foreground tabular-nums">
            {formatRelativeShort(
              new Date(operation.startedAt).toISOString(),
              now
            )}
          </p>
        </div>
        <div className="mt-1 flex min-w-0 items-center gap-2">
          <Badge
            variant="secondary"
            className="shrink-0 px-1.5 py-0 text-[11px] font-medium"
          >
            Running
          </Badge>
          {operation.context && (
            <span className="truncate text-xs text-muted-foreground">
              {operation.context}
            </span>
          )}
        </div>
        <p className="mt-1 text-xs text-muted-foreground">
          This tab only — not kept after a refresh.
        </p>
      </div>
    </div>
  )
}

export function OperationsTraySkeleton({
  rows = 3,
  label = 'Loading operations',
}: {
  rows?: number
  label?: string
}) {
  const indexes = Array.from({ length: rows }, (_, index) => index)
  return (
    <div aria-busy="true" aria-label={label}>
      {indexes.map((index) => (
        <div
          key={index}
          className={cn(
            'flex items-start gap-3 px-3 py-3',
            index > 0 && 'border-t'
          )}
        >
          <Skeleton className="size-6 shrink-0 rounded-full" />
          <div className="min-w-0 flex-1 space-y-2">
            <div className="flex items-center justify-between gap-2">
              <Skeleton className="h-4 w-40" />
              <Skeleton className="h-3 w-10" />
            </div>
            <Skeleton className="h-3 w-28" />
          </div>
        </div>
      ))}
    </div>
  )
}

function OperationsTrayError({
  title,
  message,
  onRetry,
}: {
  title: string
  message?: string | null
  onRetry: () => void
}) {
  return (
    <div className="flex flex-col items-center gap-2 px-3 py-6 text-center">
      <AlertCircle className="size-4 text-destructive" aria-hidden="true" />
      <p className="text-sm font-medium">{title}</p>
      {message && (
        <p className="line-clamp-3 text-xs text-muted-foreground">{message}</p>
      )}
      <Button variant="outline" size="sm" onClick={onRetry}>
        Retry
      </Button>
    </div>
  )
}

function OperationsTrayEmpty() {
  return (
    <div className="px-3 py-8 text-center">
      <p className="text-sm text-muted-foreground">
        {OPERATIONS_EMPTY_MESSAGE}
      </p>
    </div>
  )
}
