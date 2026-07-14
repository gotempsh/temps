'use client'

import {
  attachScheduleServicesMutation,
  deleteBackupScheduleMutation,
  detachScheduleServiceMutation,
  disableBackupScheduleMutation,
  enableBackupScheduleMutation,
  getBackupScheduleOptions,
  getS3SourceOptions,
  listScheduleServicesOptions,
  listScheduleServicesQueryKey,
} from '@/api/client/@tanstack/react-query.gen'
import { BackupScheduleResponse } from '@/api/client/types.gen'
import { ScheduleServicesSelector } from '@/components/backups/ScheduleServicesSelector'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { TooltipProvider } from '@/components/ui/tooltip'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  cleanupExpiredBackups,
  type RetentionCleanupReport,
} from '@/lib/backup-cleanup'
import {
  listScheduleRunsOptions,
  runScheduleNow,
  type ScheduleRunSummary,
} from '@/lib/schedule-runs'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ArrowLeft,
  CalendarDays,
  ChevronLeft,
  ChevronRight,
  Database,
  DatabaseBackup,
  Eraser,
  HardDrive,
  Loader2,
  MoreHorizontal,
  Pencil,
  Play,
  Plus,
  Trash2,
  X,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

// ── Local helpers ─────────────────────────────────────────────────────────────

/** Duration from ms: "1h 5m", "32m 10s", "45s", or "—" for invalid. */
function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '—'
  const seconds = Math.floor(ms / 1000)
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  const remSeconds = seconds % 60
  if (minutes < 60)
    return remSeconds ? `${minutes}m ${remSeconds}s` : `${minutes}m`
  const hours = Math.floor(minutes / 60)
  const remMinutes = minutes % 60
  return remMinutes ? `${hours}h ${remMinutes}m` : `${hours}h`
}

/** Wall-clock timeout from seconds: "4h", "1h 30m", "20m". */
function formatTimeoutSecs(secs: number): string {
  if (secs <= 0) return '—'
  const hours = Math.floor(secs / 3600)
  const minutes = Math.floor((secs % 3600) / 60)
  if (hours > 0 && minutes > 0) return `${hours}h ${minutes}m`
  if (hours > 0) return `${hours}h`
  return `${minutes}m`
}

function stateBadgeVariant(
  state: string,
): 'default' | 'secondary' | 'destructive' | 'outline' {
  switch (state) {
    case 'completed':
      return 'default'
    case 'failed':
      return 'destructive'
    case 'running':
    case 'pending':
      return 'secondary'
    default:
      return 'outline'
  }
}

/**
 * One row in the run-history table. Renders the per-tick summary and lazily
 * fetches the child job list when expanded.
 */
function RunRow({ run }: { run: ScheduleRunSummary }) {
  // Negative `run_id` are synthetic legacy rows (pre-fan-out backups). They
  // have no run-detail page, so render as a plain non-linked row.
  const isLegacy = run.run_id < 0

  const durationMs =
    run.started_at && run.finished_at
      ? new Date(run.finished_at).getTime() - new Date(run.started_at).getTime()
      : null

  const detailUrl = `/backups/schedules/${run.schedule_id}/runs/${run.run_id}`

  return (
    <TableRow className="hover:bg-muted/50">
      <TableCell className="font-mono text-xs">
        {isLegacy ? (
          <>
            <span className="sm:hidden">
              {format(new Date(run.started_at), 'MMM d HH:mm')}
            </span>
            <span className="hidden sm:inline">
              {format(new Date(run.started_at), 'MMM d, yyyy HH:mm:ss')}
            </span>
          </>
        ) : (
          <Link to={detailUrl} className="flex hover:underline">
            <span className="sm:hidden">
              {format(new Date(run.started_at), 'MMM d HH:mm')}
            </span>
            <span className="hidden sm:inline">
              {format(new Date(run.started_at), 'MMM d, yyyy HH:mm:ss')}
            </span>
          </Link>
        )}
      </TableCell>

      <TableCell className="hidden text-base text-muted-foreground sm:table-cell sm:text-sm">
        {durationMs !== null
          ? formatDuration(durationMs)
          : run.aggregate_state === 'running' ||
              run.aggregate_state === 'pending'
            ? '…'
            : '—'}
      </TableCell>

      <TableCell>
        <Badge variant={stateBadgeVariant(run.aggregate_state)}>
          {run.aggregate_state}
        </Badge>
      </TableCell>

      <TableCell className="hidden text-base text-muted-foreground md:table-cell md:text-sm">
        {run.triggered_by}
      </TableCell>

      <TableCell className="text-base sm:text-sm">
        {run.failed_jobs > 0 ? (
          <span className="text-destructive">
            {run.completed_jobs} / {run.total_jobs}
            <span className="hidden sm:inline">
              {' '}
              (<span className="font-medium">{run.failed_jobs} failed</span>)
            </span>
            <span className="ml-1 font-medium sm:hidden">
              · {run.failed_jobs} failed
            </span>
          </span>
        ) : (
          <span className="text-muted-foreground">
            {run.completed_jobs} / {run.total_jobs}
          </span>
        )}
      </TableCell>
    </TableRow>
  )
}

// ── Component ─────────────────────────────────────────────────────────────────

export function ScheduleDetail() {
  const { id } = useParams<{ id: string }>()
  const scheduleId = id ? parseInt(id) : undefined
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()

  const [page, setPage] = useState(1)
  const pageSize = 20

  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const [showCleanupDialog, setShowCleanupDialog] = useState(false)
  const [cleanupReport, setCleanupReport] = useState<RetentionCleanupReport | null>(null)
  const [showAttachDialog, setShowAttachDialog] = useState(false)
  const [pendingAttachIds, setPendingAttachIds] = useState<number[]>([])

  // ── Data fetching ──────────────────────────────────────────────────────────

  const { data: schedule, isLoading: isLoadingSchedule } = useQuery({
    ...getBackupScheduleOptions({ path: { id: scheduleId! } }),
    enabled: !!scheduleId,
  })

  const { data: s3Source } = useQuery({
    ...getS3SourceOptions({ path: { id: schedule?.s3_source_id ?? 0 } }),
    enabled: !!schedule?.s3_source_id,
  })

  const {
    data: runsData,
    isLoading: isLoadingRuns,
  } = useQuery({
    ...listScheduleRunsOptions(scheduleId, page, pageSize),
  })

  // ── Mutations ──────────────────────────────────────────────────────────────

  const runNowMutation = useMutation({
    mutationFn: () => runScheduleNow(scheduleId!),
    onSuccess: (data) => {
      toast.success('Run enqueued', {
        description: `Scheduler run #${data.schedule_run_id} fan-out: ${data.jobs.length} job${data.jobs.length === 1 ? '' : 's'} pending.`,
      })
      void queryClient.invalidateQueries({
        queryKey: ['schedule-runs', scheduleId],
      })
      // Navigate to the new run detail page so the user sees per-job progress.
      navigate(`/backups/schedules/${scheduleId}/runs/${data.schedule_run_id}`)
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Failed to enqueue backup', { description: message })
    },
  })

  const disableMutation = useMutation({
    ...disableBackupScheduleMutation(),
    onSuccess: () => {
      toast.success('Schedule disabled')
      void queryClient.invalidateQueries({
        queryKey: getBackupScheduleOptions({ path: { id: scheduleId! } }).queryKey,
      })
    },
    onError: () => toast.error('Failed to disable schedule'),
  })

  const enableMutation = useMutation({
    ...enableBackupScheduleMutation(),
    onSuccess: () => {
      toast.success('Schedule enabled')
      void queryClient.invalidateQueries({
        queryKey: getBackupScheduleOptions({ path: { id: scheduleId! } }).queryKey,
      })
    },
    onError: () => toast.error('Failed to enable schedule'),
  })

  const deleteMutation = useMutation({
    ...deleteBackupScheduleMutation(),
    onSuccess: () => {
      toast.success('Schedule deleted')
      navigate('/backups')
    },
    onError: () => toast.error('Failed to delete schedule'),
    onSettled: () => setShowDeleteDialog(false),
  })

  const cleanupPreviewMutation = useMutation({
    mutationFn: () => cleanupExpiredBackups({ dryRun: true, scheduleId: scheduleId! }),
    onSuccess: setCleanupReport,
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Cleanup preview failed', { description: message })
    },
  })

  const cleanupExecuteMutation = useMutation({
    mutationFn: (expectedBackupIds: string[]) =>
      cleanupExpiredBackups({ scheduleId: scheduleId!, expectedBackupIds }),
    onSuccess: (report) => {
      setCleanupReport(report)
      if (report.failed === 0) {
        toast.success('Expired backups deleted', {
          description: `${report.deleted} backup${report.deleted === 1 ? '' : 's'} removed by this schedule's retention policy.`,
        })
      } else {
        toast.warning('Cleanup completed with failures', {
          description: `${report.deleted} deleted, ${report.failed} failed.`,
        })
      }
      void queryClient.invalidateQueries({ queryKey: ['schedule-runs', scheduleId] })
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Cleanup failed', { description: message })
    },
  })

  const openCleanupDialog = () => {
    setCleanupReport(null)
    setShowCleanupDialog(true)
    cleanupPreviewMutation.mutate()
  }

  // ── Service attachment ────────────────────────────────────────────────────

  const { data: attachedServices, isLoading: isLoadingServices } = useQuery({
    ...listScheduleServicesOptions({ path: { id: scheduleId! } }),
    enabled: !!scheduleId,
  })

  const attachMutation = useMutation({
    ...attachScheduleServicesMutation(),
    meta: { errorTitle: 'Failed to attach services' },
    onSuccess: () => {
      toast.success('Services attached')
      void queryClient.invalidateQueries({
        queryKey: listScheduleServicesQueryKey({
          path: { id: scheduleId! },
        }),
      })
      setShowAttachDialog(false)
      setPendingAttachIds([])
    },
  })

  const detachMutation = useMutation({
    ...detachScheduleServiceMutation(),
    meta: { errorTitle: 'Failed to detach service' },
    onSuccess: () => {
      toast.success('Service detached')
      void queryClient.invalidateQueries({
        queryKey: listScheduleServicesQueryKey({
          path: { id: scheduleId! },
        }),
      })
    },
  })

  // ── Breadcrumbs ────────────────────────────────────────────────────────────

  useEffect(() => {
    if (!schedule) return
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      {
        label: s3Source?.name ?? `S3 Source ${schedule.s3_source_id}`,
        href: `/backups/s3-sources/${schedule.s3_source_id}`,
      },
      { label: schedule.name },
    ])
  }, [setBreadcrumbs, schedule, s3Source])

  usePageTitle(schedule?.name ?? 'Schedule Detail')

  // ── Pagination helpers ─────────────────────────────────────────────────────

  const total = runsData?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  const rangeStart = (page - 1) * pageSize + 1
  const rangeEnd = Math.min(page * pageSize, total)

  // ── Render helpers ─────────────────────────────────────────────────────────

  function renderScheduleConfigCard(s: BackupScheduleResponse) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <CalendarDays className="h-5 w-5" />
            Schedule Configuration
          </CardTitle>
          <CardDescription>Current settings for this schedule</CardDescription>
        </CardHeader>
        <CardContent>
          <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Cron expression
              </dt>
              <dd className="break-all font-mono text-base sm:text-sm">
                {s.schedule_expression}
              </dd>
            </div>
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Backup type
              </dt>
              <dd>
                <Badge variant="outline">{s.backup_type}</Badge>
              </dd>
            </div>
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Retention
              </dt>
              <dd className="text-base sm:text-sm">
                {s.retention_period} days
              </dd>
            </div>
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Max runtime
              </dt>
              <dd className="text-base text-muted-foreground sm:text-sm">
                {s.max_runtime_secs
                  ? formatTimeoutSecs(s.max_runtime_secs)
                  : 'engine default'}
              </dd>
            </div>
            {s.description && (
              <div className="sm:col-span-2">
                <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                  Description
                </dt>
                <dd className="text-base sm:text-sm">{s.description}</dd>
              </div>
            )}
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Last run
              </dt>
              <dd className="text-base sm:text-sm">
                {s.last_run
                  ? format(new Date(s.last_run), 'MMM d, yyyy HH:mm')
                  : '—'}
              </dd>
            </div>
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Next run
              </dt>
              <dd className="text-base sm:text-sm">
                {s.next_run
                  ? format(new Date(s.next_run), 'MMM d, yyyy HH:mm')
                  : '—'}
              </dd>
            </div>
            {s3Source && (
              <div>
                <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                  S3 source
                </dt>
                <dd>
                  <Link
                    to={`/backups/s3-sources/${s.s3_source_id}`}
                    className="text-base text-primary hover:underline sm:text-sm"
                  >
                    {s3Source.name}
                  </Link>
                </dd>
              </div>
            )}
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Backup targets
              </dt>
              <dd className="text-base sm:text-sm">
                {s.target_all_services ? (
                  <span>
                    All databases{' '}
                    <span className="text-muted-foreground">
                      (includes future databases automatically)
                    </span>
                  </span>
                ) : (
                  <span>Specific databases (configured below)</span>
                )}
              </dd>
            </div>
            <div>
              <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                Control plane backup
              </dt>
              <dd className="text-base sm:text-sm">
                {s.include_control_plane ? (
                  <span>
                    Included{' '}
                    <span className="text-muted-foreground">
                      (Temps&apos;s own database is backed up every run)
                    </span>
                  </span>
                ) : (
                  <span>
                    Skipped{' '}
                    <span className="text-muted-foreground">
                      (only external services are backed up)
                    </span>
                  </span>
                )}
              </dd>
            </div>
          </dl>
        </CardContent>
      </Card>
    )
  }

  // ── Loading state ──────────────────────────────────────────────────────────

  if (isLoadingSchedule) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-48 w-full" />
        <Skeleton className="h-96 w-full" />
      </div>
    )
  }

  if (!schedule) {
    return (
      <div className="flex flex-col items-center justify-center px-4 py-12 text-center">
        <h2 className="text-lg font-semibold">Schedule Not Found</h2>
        <p className="mt-1 text-base text-muted-foreground sm:text-sm">
          The requested backup schedule could not be found.
        </p>
        <Button asChild className="mt-4">
          <Link to="/backups">
            <ArrowLeft className="mr-2 h-4 w-4" />
            Back to Backups
          </Link>
        </Button>
      </div>
    )
  }

  // ── Main render ────────────────────────────────────────────────────────────

  return (
    <TooltipProvider>
      <div className="space-y-6">
        {/* ── Header ── */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex min-w-0 items-start gap-2 sm:items-center sm:gap-3">
            <Button
              variant="ghost"
              size="icon"
              className="-ml-1 shrink-0 sm:hidden"
              asChild
              aria-label="Back"
            >
              <Link to={`/backups/s3-sources/${schedule.s3_source_id}`}>
                <ArrowLeft className="h-4 w-4" />
              </Link>
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="hidden shrink-0 sm:inline-flex"
              asChild
            >
              <Link to={`/backups/s3-sources/${schedule.s3_source_id}`}>
                <ArrowLeft className="mr-2 h-4 w-4" />
                Back
              </Link>
            </Button>
            <div className="min-w-0">
              <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                <DatabaseBackup className="h-5 w-5 shrink-0 text-muted-foreground" />
                <h1 className="break-words text-xl font-semibold">
                  {schedule.name}
                </h1>
                <Badge variant={schedule.enabled ? 'default' : 'secondary'}>
                  {schedule.enabled ? 'Enabled' : 'Disabled'}
                </Badge>
              </div>
            </div>
          </div>

          {/* ── Action row ── */}
          <div className="flex items-center gap-2 sm:shrink-0">
            <Button
              variant="default"
              size="sm"
              className="shrink-0"
              disabled={!schedule.enabled || runNowMutation.isPending}
              onClick={() => runNowMutation.mutate()}
              aria-label="Run now"
              title={
                !schedule.enabled
                  ? 'Enable the schedule before running'
                  : 'Enqueue a backup immediately'
              }
            >
              {runNowMutation.isPending ? (
                <Loader2 className="h-4 w-4 animate-spin sm:mr-2" />
              ) : (
                <Play className="h-4 w-4 sm:mr-2" />
              )}
              <span className="hidden sm:inline">Run now</span>
            </Button>

            <Button
              variant="outline"
              size="sm"
              className="shrink-0"
              asChild
              title="Edit schedule"
            >
              <Link
                to={`/backups/s3-sources/${schedule.s3_source_id}/schedules/${schedule.id}/edit`}
                aria-label="Edit schedule"
              >
                <Pencil className="h-4 w-4 sm:mr-2" />
                <span className="hidden sm:inline">Edit</span>
              </Link>
            </Button>

            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="outline"
                  size="sm"
                  className="shrink-0"
                  aria-label="More actions"
                >
                  <MoreHorizontal className="h-4 w-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem
                  onClick={() => {
                    if (schedule.enabled) {
                      disableMutation.mutate({ path: { id: schedule.id } })
                    } else {
                      enableMutation.mutate({ path: { id: schedule.id } })
                    }
                  }}
                  disabled={
                    disableMutation.isPending || enableMutation.isPending
                  }
                >
                  {schedule.enabled ? 'Disable' : 'Enable'}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  onClick={openCleanupDialog}
                  disabled={cleanupPreviewMutation.isPending}
                >
                  <Eraser className="mr-2 h-4 w-4" />
                  Clean up expired backups
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  className="text-destructive"
                  onClick={() => setShowDeleteDialog(true)}
                >
                  <Trash2 className="mr-2 h-4 w-4" />
                  Delete
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>

        {/* ── Config card ── */}
        {renderScheduleConfigCard(schedule)}

        {/* ── Backup targets card ── */}
        {/*
         * In 'all databases' mode the join table is irrelevant — the
         * fan-out targets every external service at run time. Show a
         * hint instead of the attach/detach UI to avoid implying that
         * any of those buttons would change behaviour. In 'specific'
         * mode we surface the editable list.
         */}
        {schedule.target_all_services ? (
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Database className="h-5 w-5" />
                Backup targets
              </CardTitle>
              <CardDescription>
                This schedule backs up every database on the host. New
                databases are automatically included on the next run.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
                To restrict this schedule to a specific list of databases,
                edit the schedule and switch to <strong>Specific
                databases</strong>.
              </div>
            </CardContent>
          </Card>
        ) : (
        <Card>
          <CardHeader className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <CardTitle className="flex items-center gap-2">
                <Database className="h-5 w-5" />
                Backup targets
              </CardTitle>
              <CardDescription>
                External services this schedule backs up on every run.
                Currently in <strong>specific</strong> mode — only the
                listed services are included.
              </CardDescription>
            </div>
            <Button
              size="sm"
              variant="outline"
              onClick={() => {
                setPendingAttachIds([])
                setShowAttachDialog(true)
              }}
              className="shrink-0"
            >
              <Plus className="h-4 w-4 sm:mr-2" />
              <span className="hidden sm:inline">Attach service</span>
            </Button>
          </CardHeader>
          <CardContent>
            {isLoadingServices ? (
              <div className="space-y-2">
                <Skeleton className="h-10 w-full" />
                <Skeleton className="h-10 w-full" />
              </div>
            ) : !attachedServices || attachedServices.length === 0 ? (
              <div className="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground">
                No services attached yet. Click <strong>Attach service</strong>{' '}
                to add Postgres, Redis, MongoDB, or RustFS targets.
              </div>
            ) : (
              <ul className="divide-y rounded-md border">
                {attachedServices.map((svc) => {
                  const Icon = svc.service_type === 's3' ? HardDrive : Database
                  return (
                    <li
                      key={svc.id}
                      className="flex items-center gap-3 px-3 py-2 text-sm"
                    >
                      <Icon
                        className="h-4 w-4 text-muted-foreground"
                        aria-hidden
                      />
                      <span className="flex-1 truncate">{svc.name}</span>
                      <Badge variant="outline" className="text-xs">
                        {svc.service_type}
                      </Badge>
                      <Button
                        variant="ghost"
                        size="icon"
                        className="h-7 w-7 text-muted-foreground hover:text-destructive"
                        disabled={detachMutation.isPending}
                        onClick={() =>
                          detachMutation.mutate({
                            path: {
                              id: scheduleId!,
                              service_id: svc.id,
                            },
                          })
                        }
                        aria-label={`Detach ${svc.name}`}
                      >
                        <X className="h-4 w-4" />
                      </Button>
                    </li>
                  )
                })}
              </ul>
            )}
          </CardContent>
        </Card>
        )}

        {/* ── Run history table ── */}
        <Card>
          <CardHeader>
            <CardTitle>Run History</CardTitle>
            <CardDescription>
              Backup runs for this schedule, newest first. Tap a row for full
              details.
            </CardDescription>
          </CardHeader>
          <CardContent className="p-0">
            {isLoadingRuns ? (
              <div className="space-y-3 p-6">
                {[...Array(5)].map((_, i) => (
                  <Skeleton key={i} className="h-10 w-full" />
                ))}
              </div>
            ) : !runsData || runsData.runs.length === 0 ? (
              <div className="flex flex-col items-center justify-center px-6 py-12 text-center text-base text-muted-foreground sm:text-sm">
                <DatabaseBackup className="mb-3 h-8 w-8 opacity-40" />
                No runs yet — tap &ldquo;Run now&rdquo; to start the first
                backup.
              </div>
            ) : (
              <div className="overflow-x-auto">
                <Table className="min-w-[480px]">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Started</TableHead>
                      <TableHead className="hidden sm:table-cell">
                        Duration
                      </TableHead>
                      <TableHead>State</TableHead>
                      <TableHead className="hidden md:table-cell">
                        Trigger
                      </TableHead>
                      <TableHead>Jobs</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {runsData.runs.map((run) => (
                      <RunRow key={run.run_id} run={run} />
                    ))}
                  </TableBody>
                </Table>
              </div>
            )}

            {/* ── Pagination ── */}
            {runsData && runsData.total > 0 && (
              <div className="flex flex-wrap items-center justify-between gap-2 border-t px-4 py-3 sm:px-6 sm:py-4">
                <span className="hidden text-sm text-muted-foreground sm:inline">
                  Showing {rangeStart}–{rangeEnd} of {total}
                </span>
                <span className="text-base text-muted-foreground sm:hidden sm:text-sm">
                  {page} / {totalPages}
                </span>
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={page <= 1}
                    onClick={() => setPage((p) => Math.max(1, p - 1))}
                    aria-label="Previous page"
                  >
                    <ChevronLeft className="h-4 w-4" />
                    <span className="hidden sm:ml-1 sm:inline">Previous</span>
                  </Button>
                  <span className="hidden text-sm sm:inline">
                    {page} / {totalPages}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={page >= totalPages}
                    onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
                    aria-label="Next page"
                  >
                    <span className="hidden sm:mr-1 sm:inline">Next</span>
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      {/* ── Attach services dialog ── */}
      <Dialog open={showAttachDialog} onOpenChange={setShowAttachDialog}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>Attach services</DialogTitle>
            <DialogDescription>
              Pick the external services to add to this schedule. Already-
              attached services are hidden.
            </DialogDescription>
          </DialogHeader>
          <ScheduleServicesSelector
            value={pendingAttachIds}
            onChange={setPendingAttachIds}
            excludeIds={attachedServices?.map((s) => s.id) ?? []}
            disabled={attachMutation.isPending}
          />
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowAttachDialog(false)}
              disabled={attachMutation.isPending}
            >
              Cancel
            </Button>
            <Button
              onClick={() =>
                attachMutation.mutate({
                  path: { id: scheduleId! },
                  body: { service_ids: pendingAttachIds },
                })
              }
              disabled={
                attachMutation.isPending || pendingAttachIds.length === 0
              }
            >
              {attachMutation.isPending ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : null}
              Attach
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ── Retention cleanup preview and confirmation ── */}
      <Dialog
        open={showCleanupDialog}
        onOpenChange={(open) => {
          if (!cleanupExecuteMutation.isPending) setShowCleanupDialog(open)
        }}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>Clean up expired backups</DialogTitle>
            <DialogDescription>
              Preview backups older than {schedule.retention_period} days for this schedule before
              permanently removing their stored data and history records.
            </DialogDescription>
          </DialogHeader>

          {cleanupPreviewMutation.isPending && !cleanupReport ? (
            <div className="flex items-center justify-center gap-3 rounded-lg border border-dashed py-10 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              Checking the retention policy…
            </div>
          ) : cleanupReport?.dry_run ? (
            <div className="space-y-4">
              <div className="rounded-lg border bg-muted/40 p-4">
                <p className="text-sm font-medium">
                  {cleanupReport.expired === 0
                    ? 'Nothing to clean up'
                    : `${cleanupReport.expired} expired backup${cleanupReport.expired === 1 ? '' : 's'} found`}
                </p>
                <p className="mt-1 text-sm text-muted-foreground">
                  {cleanupReport.expired === 0
                    ? 'Every backup is still within this schedule’s retention window.'
                    : 'This was a dry run. No objects or history records have been changed.'}
                </p>
              </div>

              {cleanupReport.candidate_backup_ids.length > 0 && (
                <div>
                  <p className="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                    Backups to delete
                  </p>
                  <ul className="max-h-48 divide-y overflow-y-auto rounded-md border bg-background font-mono text-xs">
                    {cleanupReport.candidate_backup_ids.map((backupId) => (
                      <li key={backupId} className="break-all px-3 py-2">
                        {backupId}
                      </li>
                    ))}
                  </ul>
                  {cleanupReport.candidate_backup_ids_truncated && (
                    <p className="mt-2 text-xs text-muted-foreground">
                      Showing the first 100 candidates. Cleanup removes only the backups shown;
                      preview again afterward for the remaining backups.
                    </p>
                  )}
                </div>
              )}
            </div>
          ) : cleanupReport ? (
            <div className="space-y-3 rounded-lg border bg-muted/40 p-4 text-sm">
              <p className="font-medium">Cleanup finished</p>
              <p className="text-muted-foreground">
                {cleanupReport.deleted} deleted, {cleanupReport.failed} failed.
              </p>
              {cleanupReport.failures.map((failure) => (
                <p key={failure.backup_id} className="text-destructive">
                  {failure.backup_id}: {failure.reason}
                </p>
              ))}
            </div>
          ) : (
            <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-4 text-sm text-destructive">
              The preview could not be loaded. Close this dialog and try again.
            </div>
          )}

          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowCleanupDialog(false)}
              disabled={cleanupExecuteMutation.isPending}
            >
              {cleanupReport && !cleanupReport.dry_run ? 'Close' : 'Cancel'}
            </Button>
            {cleanupReport?.dry_run && cleanupReport.expired > 0 && (
              <Button
                variant="destructive"
                onClick={() =>
                  cleanupExecuteMutation.mutate(cleanupReport.candidate_backup_ids)
                }
                disabled={cleanupExecuteMutation.isPending}
              >
                {cleanupExecuteMutation.isPending ? (
                  <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                ) : (
                  <Trash2 className="mr-2 h-4 w-4" />
                )}
                Delete {cleanupReport.candidate_backup_ids.length} shown backup
                {cleanupReport.candidate_backup_ids.length === 1 ? '' : 's'}
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ── Delete confirmation dialog ── */}
      <AlertDialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete schedule?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete &ldquo;{schedule.name}&rdquo;. Existing backup
              records are not deleted, but no new backups will be scheduled.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              onClick={() =>
                deleteMutation.mutate({ path: { id: schedule.id } })
              }
              disabled={deleteMutation.isPending}
            >
              {deleteMutation.isPending ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : null}
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </TooltipProvider>
  )
}
