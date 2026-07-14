'use client'

import {
  getBackupOptions,
  getS3SourceOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
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
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import { CopyButton } from '@/components/ui/copy-button'
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
import { listBackupChildrenOptions } from '@/lib/backup-children'
import { deleteBackup } from '@/lib/backup-cleanup'
import { cancelBackup } from '@/lib/schedule-runs'
import { cn, formatBytes } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
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
import { Link, useNavigate, useParams } from 'react-router-dom'
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
      return <CheckCircle2 className={cn(iconClass, 'text-emerald-600')} />
    case 'failed':
      return <XCircle className={cn(iconClass, 'text-destructive')} />
    case 'running':
      return <Loader2 className={cn(iconClass, 'animate-spin text-blue-600')} />
    default:
      return <Clock className={cn(iconClass, 'text-muted-foreground')} />
  }
}

function StatusBadge({ state }: { state: BackupState }) {
  // Leading-icon padding per design guidelines: vertical = left padding,
  // larger right padding for visual balance. Shared across every variant
  // so all the state pills size identically in the table.
  const base = 'gap-1.5 py-1 pl-1 pr-2 capitalize font-normal'
  switch (state) {
    case 'completed':
      return (
        <Badge
          variant="outline"
          className={cn(
            base,
            'border-emerald-600/30 bg-emerald-50 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300'
          )}
        >
          <StatusIcon state={state} />
          Completed
        </Badge>
      )
    case 'failed':
      // Muted red — tinted background + outlined border + colored text.
      // The solid `destructive` variant clashes with the destructive-red
      // error message that always sits in the next column for failed rows.
      return (
        <Badge
          variant="outline"
          className={cn(
            base,
            'border-destructive/30 bg-destructive/10 text-destructive dark:bg-destructive/20 dark:text-red-300'
          )}
        >
          <StatusIcon state={state} />
          Failed
        </Badge>
      )
    case 'running':
      return (
        <Badge
          variant="outline"
          className={cn(
            base,
            'border-blue-600/30 bg-blue-50 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300'
          )}
        >
          <StatusIcon state={state} />
          Running
        </Badge>
      )
    default:
      return (
        <Badge
          variant="outline"
          className={cn(
            base,
            'border-muted-foreground/20 bg-muted/50 text-muted-foreground'
          )}
        >
          <StatusIcon state={state} />
          {state}
        </Badge>
      )
  }
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

function Detail({
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
    <div className="grid grid-cols-1 gap-1 py-3 sm:grid-cols-3 sm:gap-4">
      <dt className="text-sm font-medium text-foreground">{label}</dt>
      <dd className="flex min-w-0 items-center gap-2 sm:col-span-2">
        <div
          className={cn(
            'min-w-0 flex-1 break-all text-sm text-muted-foreground',
            mono && 'font-mono'
          )}
        >
          {children}
        </div>
        {copy ? (
          <CopyButton value={copy} minimal className="h-8 w-8 shrink-0" />
        ) : null}
      </dd>
    </div>
  )
}

export function BackupDetail() {
  const { id, backupId } = useParams<{ id: string; backupId: string }>()
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  const { data: backup, isLoading } = useQuery({
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
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Failed to delete backup', { description: message })
    },
  })

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-16">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (!backup) {
    return (
      <div className="flex flex-col items-center justify-center py-16">
        <h2 className="text-lg font-semibold">Backup not found</h2>
        <p className="mt-1 text-sm text-muted-foreground">
          The requested backup could not be found.
        </p>
        <Button asChild className="mt-4">
          <Link to={`/backups/s3-sources/${id}`}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to S3 Source
          </Link>
        </Button>
      </div>
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

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <Button variant="ghost" size="sm" asChild>
          <Link to={`/backups/s3-sources/${id}`}>
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back
          </Link>
        </Button>
      </div>

      <div className="grid gap-6">
        {/* Overview */}
        <Card>
          <CardHeader>
            <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
              <div className="space-y-1.5 min-w-0">
                <CardTitle className="flex items-center gap-2 min-w-0">
                  <FileArchive className="h-5 w-5 shrink-0" />
                  <span className="truncate">
                    Backup{' '}
                    <span className="font-mono text-base text-muted-foreground">
                      #{backup.backup_id.slice(0, 8)}
                    </span>
                  </span>
                </CardTitle>
                <CardDescription className="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <span className="inline-flex items-center gap-1.5">
                    <Clock className="h-3.5 w-3.5" />
                    <TimeAgo date={backup.started_at} />
                  </span>
                  <span aria-hidden>·</span>
                  <span className="hidden sm:inline">
                    {format(startedAt, 'PPp')}
                  </span>
                  <span className="sm:hidden">
                    {format(startedAt, 'MMM d, p')}
                  </span>
                  {source ? (
                    <>
                      <span aria-hidden>·</span>
                      <Link
                        to={`/backups/s3-sources/${id}`}
                        className="text-foreground hover:underline truncate"
                      >
                        {source.name}
                      </Link>
                    </>
                  ) : null}
                </CardDescription>
              </div>
              <div className="flex flex-wrap items-center gap-2 sm:shrink-0">
                <StatusBadge state={state} />
                <CopyButton value={backup.s3_location} className="gap-2">
                  Copy S3 path
                </CopyButton>
                {/* Cancel — only live backups can be cancelled. Soft cancel:
                    the DB row flips immediately + the engine sees the
                    cancellation token on its next heartbeat tick. */}
                {(state === 'pending' || state === 'running') && (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setShowCancelDialog(true)}
                    disabled={cancelMutation.isPending}
                    className="gap-2"
                  >
                    {cancelMutation.isPending ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <Ban className="h-4 w-4" />
                    )}
                    Cancel
                  </Button>
                )}
                {state !== 'pending' && state !== 'running' && (
                  <Button
                    variant="destructive"
                    size="sm"
                    onClick={() => setShowDeleteDialog(true)}
                    disabled={deleteMutation.isPending}
                    className="gap-2"
                  >
                    {deleteMutation.isPending ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <Trash2 className="h-4 w-4" />
                    )}
                    Delete
                  </Button>
                )}
              </div>
            </div>
          </CardHeader>
          <CardContent>
            <div
              role="list"
              className="grid grid-cols-2 divide-x divide-y divide-border overflow-hidden rounded-md border border-border sm:grid-cols-4 sm:divide-y-0"
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
                    <>Finished {format(completedAt, 'p')}</>
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
                    ? // Collapse `5:22 PM → 5:22 PM` to a single time when the
                      // start and end land in the same minute (the common case
                      // for sub-60s backups). Saves visual noise on narrow
                      // screens.
                      format(startedAt, 'p') === format(completedAt, 'p')
                      ? format(startedAt, 'p')
                      : `${format(startedAt, 'p')} → ${format(completedAt, 'p')}`
                    : 'Not finished'
                }
              />
              <Stat
                icon={HardDrive}
                label="Size"
                value={
                  displaySize ? (
                    <span className="inline-flex items-baseline gap-2">
                      <span>{formatBytes(displaySize)}</span>
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
                  backup.compression_type && backup.compression_type !== 'none'
                    ? `${backup.compression_type} compression`
                    : 'Uncompressed'
                }
              />
              <Stat
                icon={FileArchive}
                label="Type"
                value={<span className="capitalize">{backup.backup_type}</span>}
                sub={
                  backup.file_count
                    ? `${backup.file_count.toLocaleString()} files`
                    : undefined
                }
              />
            </div>
          </CardContent>
        </Card>

        {/* Error — only when present */}
        {backup.error_message ? (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertTitle>Backup failed</AlertTitle>
            <AlertDescription className="font-mono text-xs break-all">
              {backup.error_message}
            </AlertDescription>
          </Alert>
        ) : null}

        {/* Details */}
        <Card>
          <CardHeader>
            <CardTitle>Details</CardTitle>
            <CardDescription>
              Storage, provenance, and integrity metadata for this backup.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <dl className="divide-y divide-border">
              <Detail label="Backup ID" mono copy={backup.backup_id}>
                {backup.backup_id}
              </Detail>
              <Detail label="Location" mono copy={backup.s3_location}>
                {backup.s3_location}
              </Detail>
              {source ? (
                <Detail label="S3 source">
                  <Link
                    to={`/backups/s3-sources/${id}`}
                    className="text-foreground hover:underline"
                  >
                    {source.name}
                  </Link>{' '}
                  <span className="text-muted-foreground">
                    ({source.bucket_name})
                  </span>
                </Detail>
              ) : null}
              {backup.external_service ? (
                <Detail label="Service">
                  <Link
                    to={`/storage/${backup.external_service.id}`}
                    className="text-foreground hover:underline"
                  >
                    {backup.external_service.name}
                  </Link>{' '}
                  <span className="text-muted-foreground capitalize">
                    ({backup.external_service.service_type})
                  </span>
                </Detail>
              ) : null}
              <Detail label="Created by">
                {createdByUser ? (
                  <Link
                    to={`/settings/users/${createdByUser.id}`}
                    className="text-foreground hover:underline"
                  >
                    {createdByLabel}
                  </Link>
                ) : (
                  createdByLabel
                )}
              </Detail>
              <Detail label="Started at">{format(startedAt, 'PPpp')}</Detail>
              {completedAt ? (
                <Detail label="Finished at">
                  {format(completedAt, 'PPpp')}
                </Detail>
              ) : null}
              {state === 'running' ? (
                <Detail label="Step">
                  <span className="inline-flex items-center gap-2">
                    <Loader2 className="h-3.5 w-3.5 animate-spin text-blue-600 shrink-0" />
                    <span className="font-mono text-xs">
                      {backup.current_step ?? 'starting…'}
                    </span>
                  </span>
                </Detail>
              ) : null}
              {typeof backup.attempts === 'number' && backup.attempts > 1 ? (
                <Detail label="Attempt">
                  {backup.attempts} of {backup.max_attempts ?? '?'}
                </Detail>
              ) : null}
              {typeof backup.max_runtime_secs === 'number' ? (
                <Detail label="Timeout">
                  {formatTimeoutSecs(backup.max_runtime_secs)}
                </Detail>
              ) : null}
              {backup.expires_at ? (
                <Detail label="Expires">
                  {format(new Date(backup.expires_at), 'PPp')}{' '}
                  <span className="text-muted-foreground">
                    (<TimeAgo date={backup.expires_at} />)
                  </span>
                </Detail>
              ) : null}
              {backup.schedule_id ? (
                <Detail label="Schedule">
                  <Link
                    to={`/backups/s3-sources/${id}`}
                    className="text-foreground hover:underline"
                  >
                    Schedule #{backup.schedule_id}
                  </Link>
                </Detail>
              ) : null}
              {backup.checksum ? (
                <Detail label="Checksum" mono copy={backup.checksum}>
                  {backup.checksum}
                </Detail>
              ) : null}
            </dl>
          </CardContent>
        </Card>

        {/* Services in this backup — only shown when children exist */}
        {children.length > 0 ? (
          <Card>
            <CardHeader>
              <CardTitle>Services in this backup</CardTitle>
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
                              ? formatBytes(child.size_bytes)
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

        {/* Tags */}
        {backup.tags.length > 0 ? (
          <Card>
            <CardHeader>
              <CardTitle>Tags</CardTitle>
              <CardDescription>Labels attached to this backup.</CardDescription>
            </CardHeader>
            <CardContent>
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
      </div>

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
    </div>
  )
}
