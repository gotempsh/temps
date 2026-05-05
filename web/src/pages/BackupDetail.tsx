'use client'

import {
  getBackupOptions,
  getS3SourceOptions,
  listUsersOptions,
} from '@/api/client/@tanstack/react-query.gen'
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
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { cn, formatBytes } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertCircle,
  ArrowLeft,
  CheckCircle2,
  Clock,
  FileArchive,
  HardDrive,
  Loader2,
  XCircle,
} from 'lucide-react'
import { useEffect } from 'react'
import { Link, useParams } from 'react-router-dom'

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

function StatusIcon({ state }: { state: BackupState }) {
  const iconClass = 'h-4 w-4'
  switch (state) {
    case 'completed':
      return <CheckCircle2 className={cn(iconClass, 'text-emerald-600')} />
    case 'failed':
      return <XCircle className={cn(iconClass, 'text-destructive')} />
    case 'running':
      return (
        <Loader2 className={cn(iconClass, 'animate-spin text-blue-600')} />
      )
    default:
      return <Clock className={cn(iconClass, 'text-muted-foreground')} />
  }
}

function StatusBadge({ state }: { state: BackupState }) {
  const base = 'gap-1.5 capitalize'
  switch (state) {
    case 'completed':
      return (
        <Badge
          variant="outline"
          className={cn(
            base,
            'border-emerald-600/30 bg-emerald-50 text-emerald-700 dark:bg-emerald-950/40 dark:text-emerald-300',
          )}
        >
          <StatusIcon state={state} />
          Completed
        </Badge>
      )
    case 'failed':
      return (
        <Badge variant="destructive" className={base}>
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
            'border-blue-600/30 bg-blue-50 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300',
          )}
        >
          <StatusIcon state={state} />
          Running
        </Badge>
      )
    default:
      return (
        <Badge variant="secondary" className={base}>
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
    <div className="flex flex-col gap-1 p-4">
      <div className="flex items-center gap-2 text-xs font-medium text-muted-foreground uppercase tracking-wide">
        <Icon className="h-3.5 w-3.5" />
        {label}
      </div>
      <div className="text-2xl font-semibold tabular-nums text-foreground">
        {value}
      </div>
      {sub ? (
        <div className="text-xs text-muted-foreground tabular-nums">{sub}</div>
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
            mono && 'font-mono',
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
  const { setBreadcrumbs } = useBreadcrumbs()

  const { data: backup, isLoading } = useQuery({
    ...getBackupOptions({
      path: { id: backupId! },
    }),
    enabled: !!id && !!backupId,
  })

  const sourceId = id ? parseInt(id) : undefined
  const { data: source } = useQuery({
    ...getS3SourceOptions({ path: { id: sourceId! } }),
    enabled: !!sourceId,
  })

  const { data: users } = useQuery({
    ...listUsersOptions({ query: { include_deleted: false } }),
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      {
        label: source?.name || 'S3 Source',
        href: `/backups/s3-sources/${id}`,
      },
      { label: backup?.name || 'Backup Details' },
    ])
  }, [setBreadcrumbs, id, backup?.name, source?.name])

  usePageTitle(backup?.name || 'Backup Details')

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
  const completedAt = backup.completed_at
    ? new Date(backup.completed_at)
    : null
  const durationMs = completedAt
    ? completedAt.getTime() - startedAt.getTime()
    : null
  // Final size is authoritative once the backup completes; while still
  // running we surface `live_size_bytes` (server samples S3 listing) so
  // the user sees progress instead of an indefinite blank. The backend
  // also returns a `stalled` flag for `running` rows whose heartbeat is
  // older than 5 minutes — the worker likely died.
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
  const isStalled = backup.stalled === true

  const createdByUser = users?.find((u) => u.user.id === backup.created_by)
    ?.user
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
                <CardTitle className="flex items-center gap-2">
                  <FileArchive className="h-5 w-5 shrink-0" />
                  <span className="truncate">{backup.name}</span>
                </CardTitle>
                <CardDescription className="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <span className="inline-flex items-center gap-1.5">
                    <Clock className="h-3.5 w-3.5" />
                    <TimeAgo date={backup.started_at} />
                  </span>
                  <span aria-hidden>·</span>
                  <span>{format(startedAt, 'PPp')}</span>
                  {source ? (
                    <>
                      <span aria-hidden>·</span>
                      <Link
                        to={`/backups/s3-sources/${id}`}
                        className="text-foreground hover:underline"
                      >
                        {source.name}
                      </Link>
                    </>
                  ) : null}
                </CardDescription>
              </div>
              <div className="flex items-center gap-2 shrink-0">
                <StatusBadge state={state} />
                {isStalled ? (
                  <Badge
                    variant="outline"
                    className="border-amber-500/50 bg-amber-500/10 text-amber-700 dark:text-amber-400"
                  >
                    Stalled — worker not responding
                  </Badge>
                ) : null}
                <CopyButton
                  value={backup.s3_location}
                  className="gap-2"
                >
                  Copy S3 path
                </CopyButton>
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
                    ? `${format(startedAt, 'p')} → ${format(completedAt, 'p')}`
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

        {/* Tags */}
        {backup.tags.length > 0 ? (
          <Card>
            <CardHeader>
              <CardTitle>Tags</CardTitle>
              <CardDescription>
                Labels attached to this backup.
              </CardDescription>
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
    </div>
  )
}
