// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  getBackupOptions,
  getS3SourceOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useSensitiveActionVerification } from '@/hooks/useSensitiveActionVerification'
import { listBackupChildrenOptions } from '@/lib/backup-children'
import { deleteBackup } from '@/lib/backup-cleanup'
import { cancelBackup } from '@/lib/schedule-runs'
import { cn } from '@/lib/utils'
import {
  Button,
  Callout,
  CopyAction,
  Detail,
  PageState,
  Status,
  fmtBytes,
  fmtDateTime,
  fmtRelativeTime,
  type DetailFact,
  type StatusTone,
} from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowLeft,
  Ban,
  CheckCircle2,
  Clock,
  Database,
  FileArchive,
  HardDrive,
  Loader2,
  Trash2,
  XCircle,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

type BackupState = 'completed' | 'failed' | 'running' | (string & {})

function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '—'
  const seconds = Math.floor(ms / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  const remSeconds = seconds % 60
  if (minutes < 60) {
    return remSeconds ? `${minutes}m ${remSeconds}s` : `${minutes}m`
  }
  const hours = Math.floor(minutes / 60)
  const remMinutes = minutes % 60
  return remMinutes ? `${hours}h ${remMinutes}m` : `${hours}h`
}

/** Format a wall-clock timeout from seconds into a human-readable string. */
function formatTimeoutSecs(secs: number): string {
  if (secs <= 0) return '—'
  const hours = Math.floor(secs / 3600)
  const minutes = Math.floor((secs % 3600) / 60)
  if (hours > 0 && minutes > 0) return `${hours}h ${minutes}m`
  if (hours > 0) return `${hours}h`
  return `${minutes}m`
}

function StatusIcon({ state }: { state: BackupState }) {
  const iconClass = 'h-4 w-4'
  switch (state) {
    case 'completed':
      return <CheckCircle2 className={cn(iconClass, 'text-success')} />
    case 'failed':
      return <XCircle className={cn(iconClass, 'text-destructive')} />
    case 'running':
      return <Loader2 className={cn(iconClass, 'animate-spin text-primary')} />
    default:
      return <Clock className={cn(iconClass, 'text-muted-foreground')} />
  }
}

// Mirrors the record's own state — the Detail template's single verdict.
const BACKUP_STATE_VERDICT: Record<string, { tone: StatusTone; label: string }> = {
  completed: { tone: 'ok', label: 'Completed' },
  failed: { tone: 'error', label: 'Failed' },
  running: { tone: 'running', label: 'Running' },
  pending: { tone: 'idle', label: 'Pending' },
  cancelled: { tone: 'idle', label: 'Cancelled' },
}
function backupVerdict(state: BackupState): { tone: StatusTone; label: string } {
  return BACKUP_STATE_VERDICT[state] ?? { tone: 'idle', label: state }
}

function StatusBadge({ state }: { state: BackupState }) {
  const verdict = backupVerdict(state)
  return <Status tone={verdict.tone} label={verdict.label} />
}

function Stat({
  label,
  value,
  sub,
  icon: Icon,
}: {
  label: string
  value: React.ReactNode
  sub?: React.ReactNode
  icon: React.ComponentType<{ className?: string }>
}) {
  return (
    <div className="flex flex-col gap-1 p-3 sm:p-4 min-w-0">
      <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        <Icon className="h-3.5 w-3.5 shrink-0" />
        <span className="truncate">{label}</span>
      </div>
      <div className="text-base font-semibold tabular-nums text-foreground sm:text-lg truncate">
        {value}
      </div>
      {sub ? (
        <div className="text-xs text-muted-foreground tabular-nums truncate">
          {sub}
        </div>
      ) : null}
    </div>
  )
}

// Page-local key/value row for the "Details" aside card. Named `FieldRow`
// (not `Detail`) to avoid colliding with the `Detail` template imported
// from `@temps-sdk/ds`.
function FieldRow({
  label,
  children,
  copy,
  mono,
}: {
  label: string
  children: React.ReactNode
  copy?: string
  mono?: boolean
}) {
  return (
    <div className="grid grid-cols-1 gap-1 py-3">
      <dt className="text-sm font-medium text-foreground">{label}</dt>
      <dd className="flex min-w-0 items-center gap-2">
        <div
          className={cn(
            'min-w-0 flex-1 break-all text-sm text-muted-foreground',
            mono && 'font-mono'
          )}
        >
          {children}
        </div>
        {copy ? <CopyAction value={copy} label={`Copy ${label.toLowerCase()}`} /> : null}
      </dd>
    </div>
  )
}

function BackupDetailSkeleton({ backAction }: { backAction: React.ReactNode }) {
  return (
    <Detail
      title={<div className="h-7 w-56 animate-pulse rounded bg-muted" />}
      actions={backAction}
      facts={[0, 1, 2, 3].map(() => ({
        label: <div className="h-3 w-16 animate-pulse rounded bg-muted" />,
        value: <div className="h-4 w-24 animate-pulse rounded bg-muted" />,
      }))}
      main={<div className="h-64 w-full animate-pulse rounded-lg bg-muted" />}
      aside={<div className="h-80 w-full animate-pulse rounded-lg bg-muted" />}
    />
  )
}

export function BackupDetail() {
  const { id, backupId } = useParams<{ id: string; backupId: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  const {
    data: backup,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...getBackupOptions({
      path: { id: backupId! },
    }),
    enabled: !!id && !!backupId,
    // Poll every 5 s while the backup is running so step transitions are
    // visible without a manual refresh. Back off to no polling when done.
    refetchInterval: (query) => {
      const state = (query.state.data as { state?: string } | undefined)?.state
      return state === 'running' ? 5_000 : false
    },
  })

  const sourceId = id ? parseInt(id) : undefined
  const { data: source } = useQuery({
    ...getS3SourceOptions({ path: { id: sourceId! } }),
    enabled: !!sourceId,
  })

  const { data: users } = useQuery({
    ...listUsersOptions({ query: { include_deleted: false } }),
  })

  // Fetch child external-service backups using the integer row id once the
  // parent backup has loaded. The query is a no-op until `backup` is defined.
  const { data: childrenData } = useQuery({
    ...listBackupChildrenOptions(backup?.id),
  })
  const children = childrenData?.children ?? []

  // `backup.name` is `"Backup <full-uuid>"` which is too long for
  // breadcrumbs and the tab title. Show the friendlier short form
  // (`Backup #<first 8 chars>`) instead; the full UUID stays available
  // in the Details card with a copy button.
  const shortBackupLabel = backup?.backup_id
    ? `Backup #${backup.backup_id.slice(0, 8)}`
    : 'Backup Details'

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      {
        label: source?.name || 'S3 Source',
        href: `/backups/s3-sources/${id}`,
      },
      { label: shortBackupLabel },
    ])
  }, [setBreadcrumbs, id, shortBackupLabel, source?.name])

  usePageTitle(shortBackupLabel)

  // ── Cancel mutation ─────────────────────────────────────────────────────
  //
  // Soft cancel: flips the DB row to `failed` immediately and sets the
  // in-process cancellation token. The engine notices on the next heartbeat
  // tick (≤5s) and exits cleanly; rollback reaps any sidecar container.
  //
  // Idempotent server-side — cancelling an already-terminal backup returns
  // `cancelled: 0` which we treat as a friendly "nothing to do" toast.
  const queryClient = useQueryClient()
  const [showCancelDialog, setShowCancelDialog] = useState(false)
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const { handleSensitiveActionError, verificationDialog } =
    useSensitiveActionVerification()

  const cancelMutation = useMutation({
    mutationFn: () => cancelBackup(backup!.id),
    onSuccess: (data) => {
      toast.success('Backup cancelled', {
        description:
          data.cancelled === 0
            ? 'Backup was already terminal — nothing to cancel.'
            : 'Engine will stop on the next heartbeat tick (within ~5s).',
      })
      void queryClient.invalidateQueries({
        queryKey: ['getBackup', { path: { id: backupId! } }],
      })
      setShowCancelDialog(false)
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Failed to cancel backup', { description: message })
    },
  })

  const deleteMutation = useMutation({
    mutationFn: () => deleteBackup(backup!.backup_id),
    onSuccess: () => {
      toast.success('Backup deleted', {
        description:
          'The stored backup data and its history record were removed.',
      })
      navigate(`/backups/s3-sources/${id}`)
    },
    onError: (err: unknown) => {
      if (handleSensitiveActionError(err, () => deleteMutation.mutate())) {
        setShowDeleteDialog(false)
        return
      }
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Failed to delete backup', { description: message })
    },
  })

  const backAction = (
    <Button variant="ghost" size="sm" asChild>
      <Link to={`/backups/s3-sources/${id}`}>
        <ArrowLeft className="mr-2 h-4 w-4" />
        Back
      </Link>
    </Button>
  )

  if (isLoading) {
    return <BackupDetailSkeleton backAction={backAction} />
  }

  if (error && !backup) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Couldn't load backup"
        description={
          error instanceof Error
            ? error.message
            : 'An unexpected error occurred. Please try again.'
        }
        action={
          <div className="flex gap-2">
            <Button onClick={() => void refetch()}>Retry</Button>
            {backAction}
          </div>
        }
      />
    )
  }

  if (!backup) {
    return (
      <PageState
        variant="failed"
        icon={AlertCircle}
        title="Backup not found"
        description="The requested backup could not be found."
        action={backAction}
      />
    )
  }

  const state = backup.state as BackupState
  const startedAt = new Date(backup.started_at)
  const completedAt = backup.completed_at ? new Date(backup.completed_at) : null
  const durationMs = completedAt
    ? completedAt.getTime() - startedAt.getTime()
    : null
  // Final size is authoritative once the backup completes; while still
  // running we surface `live_size_bytes` (server samples S3 listing) so
  // the user sees progress instead of an indefinite blank.
  const finalSize =
    typeof backup.size_bytes === 'number' && backup.size_bytes > 0
      ? backup.size_bytes
      : (backup.metadata as { size_bytes?: number } | null)?.size_bytes
  const liveSize =
    typeof backup.live_size_bytes === 'number' && backup.live_size_bytes > 0
      ? backup.live_size_bytes
      : null
  const displaySize = finalSize ?? liveSize
  const isLiveSize = !finalSize && liveSize !== null

  const createdByUser = users?.find(
    (u) => u.user.id === backup.created_by
  )?.user
  const createdByLabel = createdByUser
    ? createdByUser.name || createdByUser.username || createdByUser.email
    : `User #${backup.created_by}`

  // These are deliberately different fields from the Overview stat row
  // below (status/duration/size/type) — provenance and retention, not
  // performance, so nothing is shown twice.
  const facts: DetailFact[] = [
    {
      label: backup.external_service ? 'Service' : 'S3 source',
      value: backup.external_service ? (
        <Link to={`/storage/${backup.external_service.id}`} className="hover:underline">
          {backup.external_service.name}
        </Link>
      ) : source ? (
        <Link to={`/backups/s3-sources/${id}`} className="hover:underline">
          {source.name}
        </Link>
      ) : (
        '—'
      ),
    },
    {
      label: 'Created by',
      value: createdByUser ? (
        <Link to={`/settings/users/${createdByUser.id}`} className="hover:underline">
          {createdByLabel}
        </Link>
      ) : (
        createdByLabel
      ),
    },
    {
      label: 'Retained until',
      value: backup.expires_at ? (
        <span title={fmtDateTime(backup.expires_at)}>
          {fmtRelativeTime(backup.expires_at)}
        </span>
      ) : (
        'Kept until deleted'
      ),
    },
    {
      label: 'Compression',
      value:
        backup.compression_type && backup.compression_type !== 'none'
          ? backup.compression_type
          : 'Uncompressed',
    },
  ]

  return (
    <>
      <Detail
        title={
          <span className="flex items-center gap-2">
            <FileArchive className="h-5 w-5 shrink-0" />
            Backup{' '}
            <span className="font-mono text-base text-muted-foreground">
              #{backup.backup_id.slice(0, 8)}
            </span>
          </span>
        }
        description={
          <span className="inline-flex items-center gap-1.5">
            <Clock className="h-3.5 w-3.5" />
            <TimeAgo date={backup.started_at} />
            <span aria-hidden>·</span>
            {fmtDateTime(backup.started_at)}
          </span>
        }
        verdict={<StatusBadge state={state} />}
        facts={facts}
        actions={
          <>
            {backAction}
            <CopyAction value={backup.s3_location}>Copy S3 path</CopyAction>
            {/* Cancel — only live backups can be cancelled. Soft cancel:
              the DB row flips immediately + the engine sees the
              cancellation token on its next heartbeat tick. */}
            {(state === 'pending' || state === 'running') && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => setShowCancelDialog(true)}
                busy={cancelMutation.isPending}
                busyLabel="Cancelling…"
                className="gap-2"
              >
                <Ban className="h-4 w-4" />
                Cancel
              </Button>
            )}
            {state !== 'pending' && state !== 'running' && (
              <Button
                variant="destructive"
                size="sm"
                onClick={() => setShowDeleteDialog(true)}
                busy={deleteMutation.isPending}
                busyLabel="Deleting…"
                className="gap-2"
              >
                <Trash2 className="h-4 w-4" />
                Delete
              </Button>
            )}
          </>
        }
        main={
          <>
            <Card className="overflow-hidden shadow-none">
              <CardHeader className="border-b px-5 py-4">
                <CardTitle className="text-base font-semibold">Overview</CardTitle>
              </CardHeader>
              <CardContent className="p-0">
                <div
                  role="list"
                  className="grid grid-cols-2 divide-x divide-y divide-border overflow-hidden sm:grid-cols-4 sm:divide-y-0"
                >
                  <Stat
                    icon={CheckCircle2}
                    label="Status"
                    value={
                      <span className="inline-flex items-center gap-2 text-xl">
                        <StatusIcon state={state} />
                        <span className="capitalize">{state}</span>
                      </span>
                    }
                    sub={
                      completedAt ? (
                        <>Finished {fmtDateTime(completedAt)}</>
                      ) : state === 'running' ? (
                        <>In progress</>
                      ) : (
                        <>—</>
                      )
                    }
                  />
                  <Stat
                    icon={Clock}
                    label="Duration"
                    value={durationMs !== null ? formatDuration(durationMs) : '—'}
                    sub={
                      completedAt
                        ? `${fmtDateTime(startedAt)} → ${fmtDateTime(completedAt)}`
                        : 'Not finished'
                    }
                  />
                  <Stat
                    icon={HardDrive}
                    label="Size"
                    value={
                      displaySize ? (
                        <span className="inline-flex items-baseline gap-2">
                          <span>{fmtBytes(displaySize)}</span>
                          {isLiveSize ? (
                            <span className="text-xs font-normal text-muted-foreground">
                              so far
                            </span>
                          ) : null}
                        </span>
                      ) : (
                        '—'
                      )
                    }
                    sub={
                      backup.compression_type &&
                      backup.compression_type !== 'none'
                        ? `${backup.compression_type} compression`
                        : 'Uncompressed'
                    }
                  />
                  <Stat
                    icon={FileArchive}
                    label="Type"
                    value={
                      <span className="capitalize">{backup.backup_type}</span>
                    }
                    sub={
                      backup.file_count
                        ? `${backup.file_count.toLocaleString()} files`
                        : undefined
                    }
                  />
                </div>
              </CardContent>
            </Card>

            {backup.error_message ? (
              // A message on a still-pending backup is not a failure: the
              // server records why it could not start it here (e.g. the
              // engine needs a Docker daemon this process does not have) and
              // leaves the run outstanding. Calling that "failed" would send
              // the operator looking for a failure that never happened.
              <Callout
                tone={state === 'pending' ? 'warning' : 'error'}
                title={state === 'pending' ? 'Backup has not started' : 'Backup failed'}
              >
                <span className="break-all font-mono text-xs">{backup.error_message}</span>
              </Callout>
            ) : null}

            {children.length > 0 ? (
              <Card className="overflow-hidden shadow-none">
                <CardHeader className="border-b px-5 py-4">
                  <CardTitle className="text-base font-semibold">
                    Services in this backup
                  </CardTitle>
                  <CardDescription>
                    External services whose data was captured in this backup run.
                  </CardDescription>
                </CardHeader>
                <CardContent className="p-0">
                  <div className="overflow-x-auto">
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Service</TableHead>
                          <TableHead>Type</TableHead>
                          <TableHead>State</TableHead>
                          <TableHead className="hidden sm:table-cell">
                            Size
                          </TableHead>
                          <TableHead className="hidden md:table-cell">
                            Duration
                          </TableHead>
                          <TableHead className="hidden lg:table-cell">
                            Error
                          </TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {children.map((child) => {
                          const childStarted = new Date(child.started_at)
                          const childFinished = child.finished_at
                            ? new Date(child.finished_at)
                            : null
                          const childDurationMs = childFinished
                            ? childFinished.getTime() - childStarted.getTime()
                            : null
                          // When the parent backup has finalized but the child is
                          // still pending/running, the engine bailed before
                          // updating the child row (typical: pre-flight S3 check
                          // failed, parent was marked failed but children were
                          // never visited). Surface the parent's state + error
                          // instead of showing a stale "Pending" forever.
                          const parentFinalized =
                            state === 'failed' || state === 'cancelled'
                          const childStale =
                            child.state === 'pending' || child.state === 'running'
                          const effectiveState =
                            parentFinalized && childStale ? state : child.state
                          const effectiveError =
                            child.error_message ??
                            (parentFinalized && childStale
                              ? (backup.error_message ?? null)
                              : null)
                          return (
                            <TableRow key={child.id}>
                              <TableCell>
                                <Link
                                  to={`/storage/${child.service_id}`}
                                  className="flex items-center gap-2 hover:underline"
                                >
                                  <Database className="h-4 w-4 shrink-0 text-muted-foreground" />
                                  <span className="font-medium">
                                    {child.service_name}
                                  </span>
                                </Link>
                              </TableCell>
                              <TableCell>
                                <Badge variant="outline" className="capitalize">
                                  {child.service_type}
                                </Badge>
                              </TableCell>
                              <TableCell>
                                <StatusBadge state={effectiveState} />
                              </TableCell>
                              <TableCell className="hidden sm:table-cell text-sm text-muted-foreground">
                                {child.size_bytes !== null
                                  ? fmtBytes(child.size_bytes)
                                  : '—'}
                              </TableCell>
                              <TableCell className="hidden md:table-cell text-sm text-muted-foreground">
                                {childDurationMs !== null
                                  ? formatDuration(childDurationMs)
                                  : '—'}
                              </TableCell>
                              <TableCell className="hidden lg:table-cell max-w-[200px]">
                                {effectiveError ? (
                                  <TooltipProvider>
                                    <Tooltip>
                                      <TooltipTrigger asChild>
                                        <span className="block truncate text-xs text-destructive cursor-help">
                                          {effectiveError}
                                        </span>
                                      </TooltipTrigger>
                                      <TooltipContent
                                        side="top"
                                        className="max-w-sm whitespace-pre-wrap break-words"
                                      >
                                        {effectiveError}
                                      </TooltipContent>
                                    </Tooltip>
                                  </TooltipProvider>
                                ) : (
                                  <span className="text-muted-foreground">—</span>
                                )}
                              </TableCell>
                            </TableRow>
                          )
                        })}
                      </TableBody>
                    </Table>
                  </div>
                </CardContent>
              </Card>
            ) : null}
          </>
        }
        aside={
          <>
            <Card className="overflow-hidden shadow-none">
              <CardHeader className="border-b px-5 py-4">
                <CardTitle className="text-base font-semibold">Details</CardTitle>
                <CardDescription>
                  Storage, provenance, and integrity metadata for this backup.
                </CardDescription>
              </CardHeader>
              <CardContent className="p-5">
                <dl className="divide-y divide-border">
                  <FieldRow label="Backup ID" mono copy={backup.backup_id}>
                    {backup.backup_id}
                  </FieldRow>
                  <FieldRow label="Location" mono copy={backup.s3_location}>
                    {backup.s3_location}
                  </FieldRow>
                  <FieldRow label="Started at">{fmtDateTime(startedAt)}</FieldRow>
                  {completedAt ? (
                    <FieldRow label="Finished at">
                      {fmtDateTime(completedAt)}
                    </FieldRow>
                  ) : null}
                  {state === 'running' ? (
                    <FieldRow label="Step">
                      <span className="inline-flex items-center gap-2">
                        <Loader2 className="h-3.5 w-3.5 animate-spin text-primary shrink-0" />
                        <span className="font-mono text-xs">
                          {backup.current_step ?? 'starting…'}
                        </span>
                      </span>
                    </FieldRow>
                  ) : null}
                  {typeof backup.attempts === 'number' && backup.attempts > 1 ? (
                    <FieldRow label="Attempt">
                      {backup.attempts} of {backup.max_attempts ?? '?'}
                    </FieldRow>
                  ) : null}
                  {typeof backup.max_runtime_secs === 'number' ? (
                    <FieldRow label="Timeout">
                      {formatTimeoutSecs(backup.max_runtime_secs)}
                    </FieldRow>
                  ) : null}
                  {backup.schedule_id ? (
                    <FieldRow label="Schedule">
                      <Link
                        to={`/backups/s3-sources/${id}`}
                        className="text-foreground hover:underline"
                      >
                        Schedule #{backup.schedule_id}
                      </Link>
                    </FieldRow>
                  ) : null}
                  {backup.checksum ? (
                    <FieldRow label="Checksum" mono copy={backup.checksum}>
                      {backup.checksum}
                    </FieldRow>
                  ) : null}
                </dl>
              </CardContent>
            </Card>

            {backup.tags.length > 0 ? (
              <Card className="overflow-hidden shadow-none">
                <CardHeader className="border-b px-5 py-4">
                  <CardTitle className="text-base font-semibold">Tags</CardTitle>
                  <CardDescription>
                    Labels attached to this backup.
                  </CardDescription>
                </CardHeader>
                <CardContent className="p-5">
                  <div className="flex flex-wrap gap-2">
                    {backup.tags.map((tag) => (
                      <Badge key={tag} variant="secondary">
                        {tag}
                      </Badge>
                    ))}
                  </div>
                </CardContent>
              </Card>
            ) : null}
          </>
        }
      />

      <AlertDialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete this backup permanently?</AlertDialogTitle>
            <AlertDialogDescription>
              This removes the backup data from object storage and deletes its
              history record. This cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Keep backup
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault()
                deleteMutation.mutate()
              }}
              disabled={deleteMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? 'Deleting…' : 'Delete permanently'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Cancel-confirm dialog. Open via the header "Cancel" button. */}
      <AlertDialog
        open={showCancelDialog}
        onOpenChange={(open) => {
          if (!cancelMutation.isPending) setShowCancelDialog(open)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Cancel this backup?</AlertDialogTitle>
            <AlertDialogDescription>
              The backup will be flipped to <strong>failed</strong>. If the
              engine is mid-dump it stops on the next heartbeat tick (within ~5
              seconds) and any partial S3 object is cleaned up by rollback.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={cancelMutation.isPending}>
              Keep running
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                cancelMutation.mutate()
              }}
              disabled={cancelMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {cancelMutation.isPending ? (
                <span className="flex items-center gap-2">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  Cancelling…
                </span>
              ) : (
                'Cancel backup'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {verificationDialog}
    </>
  )
}
