'use client'

import {
  getBackupScheduleOptions,
  getS3SourceOptions,
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
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import {
  cancelBackup,
  cancelScheduleRun,
  listScheduleRunJobsOptions,
} from '@/lib/schedule-runs'
import { formatBytes } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ArrowLeft,
  Ban,
  DatabaseBackup,
  Loader2,
  MoreHorizontal,
  RefreshCw,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

// ── Local helpers ────────────────────────────────────────────────────────────

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

// ── Component ────────────────────────────────────────────────────────────────

export function ScheduleRunDetail() {
  const { scheduleId, runId } = useParams<{
    scheduleId: string
    runId: string
  }>()
  const scheduleIdNum = scheduleId ? parseInt(scheduleId) : undefined
  const runIdNum = runId ? parseInt(runId) : undefined
  const navigate = useNavigate()
  const { setBreadcrumbs } = useBreadcrumbs()

  // ── Data ─────────────────────────────────────────────────────────────────

  const { data: schedule, isPending: isSchedulePending } = useQuery({
    ...getBackupScheduleOptions({ path: { id: scheduleIdNum ?? 0 } }),
    enabled: scheduleIdNum !== undefined,
  })

  const { data: s3Source } = useQuery({
    ...getS3SourceOptions({
      path: { id: schedule?.s3_source_id ?? 0 },
    }),
    enabled: schedule?.s3_source_id !== undefined,
  })

  const queryClient = useQueryClient()

  const jobsQueryOptions = listScheduleRunJobsOptions(runIdNum, 1, 200)

  const {
    data: jobs,
    isPending: isJobsPending,
    isError: isJobsError,
    isFetching: isJobsFetching,
    error: jobsError,
  } = useQuery({
    ...jobsQueryOptions,
    // Auto-refresh every 5s while any child is still pending/running. The
    // predicate runs on every poll so the interval stops on its own once
    // the run reaches a terminal state — no manual cleanup needed.
    refetchInterval: (query) => {
      const data = query.state.data
      if (!data) return false
      return data.some((j) => j.state === 'pending' || j.state === 'running')
        ? 5000
        : false
    },
    refetchIntervalInBackground: false,
  })

  // ── Page meta ────────────────────────────────────────────────────────────

  usePageTitle(
    schedule ? `Run #${runIdNum} • ${schedule.name}` : `Run #${runIdNum}`,
  )

  useEffect(() => {
    if (!schedule) return
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      ...(s3Source
        ? [
            {
              label: s3Source.name,
              href: `/backups/s3-sources/${s3Source.id}`,
            },
          ]
        : []),
      {
        label: schedule.name,
        href: `/backups/schedules/${schedule.id}`,
      },
      { label: `Run #${runIdNum}` },
    ])
    return () => setBreadcrumbs([])
  }, [schedule, s3Source, runIdNum, setBreadcrumbs])

  // ── Cancel mutations + confirm dialogs ───────────────────────────────────

  const [showCancelRunDialog, setShowCancelRunDialog] = useState(false)
  const [jobToCancel, setJobToCancel] = useState<{
    backup_id: number
    service_name: string
  } | null>(null)

  // Cancel the whole run. Bulk-flips every pending/running child via the
  // backend `cancel_schedule_run` and closes the parent schedule_runs row.
  const cancelRunMutation = useMutation({
    mutationFn: () => cancelScheduleRun(runIdNum!),
    onSuccess: (data) => {
      toast.success('Run cancelled', {
        description:
          data.cancelled === 0
            ? 'No live jobs to cancel — run was already terminal.'
            : `Cancelled ${data.cancelled} job${data.cancelled === 1 ? '' : 's'}.`,
      })
      void queryClient.invalidateQueries({
        queryKey: jobsQueryOptions.queryKey,
      })
      setShowCancelRunDialog(false)
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Failed to cancel run', { description: message })
    },
  })

  // Cancel one job. Soft cancel: the engine sees the token on the next
  // heartbeat tick and exits cleanly. The DB row is flipped immediately so
  // the UI updates without waiting for the engine.
  const cancelJobMutation = useMutation({
    mutationFn: (backupId: number) => cancelBackup(backupId),
    onSuccess: (data, backupId) => {
      toast.success('Job cancelled', {
        description:
          data.cancelled === 0
            ? `Backup #${backupId} was already terminal.`
            : `Backup #${backupId} cancelled.`,
      })
      void queryClient.invalidateQueries({
        queryKey: jobsQueryOptions.queryKey,
      })
      setJobToCancel(null)
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Failed to cancel job', { description: message })
    },
  })

  // ── Derived state from jobs ──────────────────────────────────────────────

  const total = jobs?.length ?? 0
  const completed = jobs?.filter((j) => j.state === 'completed').length ?? 0
  const failed = jobs?.filter((j) => j.state === 'failed').length ?? 0
  const running = jobs?.filter((j) => j.state === 'running').length ?? 0
  const pending = jobs?.filter((j) => j.state === 'pending').length ?? 0

  const aggregateState =
    pending > 0 || running > 0
      ? 'running'
      : failed > 0
        ? 'failed'
        : total > 0
          ? 'completed'
          : 'pending'

  // First job's started_at as the run start; max finished_at as run end
  // (matches backend `ScheduleRunSummary` semantics).
  const startedAt = jobs?.length
    ? jobs.reduce(
        (min, j) => (j.started_at < min ? j.started_at : min),
        jobs[0].started_at,
      )
    : undefined

  const finishedAt =
    jobs?.length && jobs.every((j) => j.finished_at)
      ? jobs.reduce<string>((max, j) => {
          const f = j.finished_at!
          return f > max ? f : max
        }, jobs[0].finished_at!)
      : undefined

  const durationMs =
    startedAt && finishedAt
      ? new Date(finishedAt).getTime() - new Date(startedAt).getTime()
      : null

  // ── Render ───────────────────────────────────────────────────────────────

  if (scheduleIdNum === undefined || runIdNum === undefined) {
    return (
      <div className="px-4 py-6 text-base text-destructive sm:px-6 sm:text-sm">
        Invalid run URL — missing schedule or run id.
      </div>
    )
  }

  return (
    <TooltipProvider>
      <div className="space-y-6">
        {/* ── Header ─────────────────────────────────────────────────── */}
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="flex min-w-0 items-start gap-2 sm:items-center sm:gap-3">
            <Button
              variant="ghost"
              size="icon"
              className="-ml-1 shrink-0"
              onClick={() => navigate(`/backups/schedules/${scheduleIdNum}`)}
              aria-label="Back to schedule"
            >
              <ArrowLeft className="h-4 w-4" />
            </Button>
            <div className="min-w-0">
              <h1 className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xl font-semibold tracking-tight sm:text-2xl">
                <span>Run #{runIdNum}</span>
                {jobs && (
                  <Badge variant={stateBadgeVariant(aggregateState)}>
                    {aggregateState}
                  </Badge>
                )}
              </h1>
              {isSchedulePending ? (
                <Skeleton className="mt-1 h-4 w-48" />
              ) : schedule ? (
                <p className="mt-0.5 text-base text-muted-foreground sm:text-sm">
                  Scheduler tick for{' '}
                  <Link
                    to={`/backups/schedules/${schedule.id}`}
                    className="break-words hover:underline"
                  >
                    {schedule.name}
                  </Link>
                </p>
              ) : null}
            </div>
          </div>

          <div className="flex items-center gap-2 sm:shrink-0">
            {/* Cancel run — only when at least one child is still live.
                Refreshing while cancelling is fine, the mutation invalidates
                the jobs query on success. */}
            {(aggregateState === 'running' || aggregateState === 'pending') && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => setShowCancelRunDialog(true)}
                disabled={cancelRunMutation.isPending}
                className="shrink-0 gap-2"
                aria-label="Cancel run"
                title="Cancel run"
              >
                {cancelRunMutation.isPending ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : (
                  <Ban className="h-4 w-4" />
                )}
                <span className="hidden sm:inline">Cancel run</span>
              </Button>
            )}

            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="outline"
                  size="icon"
                  className="shrink-0"
                  onClick={() => {
                    void queryClient.invalidateQueries({
                      queryKey: jobsQueryOptions.queryKey,
                    })
                  }}
                  disabled={isJobsFetching}
                  aria-label="Refresh run"
                >
                  <RefreshCw
                    className={`h-4 w-4 ${isJobsFetching ? 'animate-spin' : ''}`}
                  />
                </Button>
              </TooltipTrigger>
              <TooltipContent side="bottom">
                {aggregateState === 'running' || aggregateState === 'pending'
                  ? 'Refresh (auto-refreshing every 5s)'
                  : 'Refresh'}
              </TooltipContent>
            </Tooltip>
          </div>
        </div>

        {/* ── Summary stats ──────────────────────────────────────────── */}
        <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
          <SummaryStat
            label="Started"
            value={
              startedAt
                ? format(new Date(startedAt), 'MMM d, yyyy HH:mm:ss')
                : '—'
            }
            mono
          />
          <SummaryStat
            label="Finished"
            value={
              finishedAt
                ? format(new Date(finishedAt), 'MMM d, yyyy HH:mm:ss')
                : aggregateState === 'running'
                  ? 'Running…'
                  : '—'
            }
            mono
          />
          <SummaryStat
            label="Duration"
            value={durationMs !== null ? formatDuration(durationMs) : '—'}
          />
          <SummaryStat
            label="Jobs"
            value={
              failed > 0 ? (
                <span>
                  {completed} / {total}{' '}
                  <span className="text-destructive">({failed} failed)</span>
                </span>
              ) : (
                <span>
                  {completed} / {total}
                </span>
              )
            }
          />
        </div>

        {/* ── Jobs table ─────────────────────────────────────────────── */}
        <Card>
          <CardHeader>
            <CardTitle>Backup jobs</CardTitle>
            <CardDescription>
              One row per backup in this scheduler tick (control plane + each
              external service).
            </CardDescription>
          </CardHeader>
          <CardContent className="px-0">
            {isJobsPending ? (
              <div className="space-y-2 px-4 pb-6 sm:px-6">
                {[...Array(4)].map((_, i) => (
                  <Skeleton key={i} className="h-10 w-full" />
                ))}
              </div>
            ) : isJobsError ? (
              <div className="px-4 pb-6 text-base text-destructive sm:px-6 sm:text-sm">
                Failed to load jobs:{' '}
                {jobsError instanceof Error
                  ? jobsError.message
                  : 'Unknown error'}
              </div>
            ) : !jobs || jobs.length === 0 ? (
              <div className="flex flex-col items-center justify-center px-4 py-12 text-center text-base text-muted-foreground sm:px-6 sm:text-sm">
                <DatabaseBackup className="mb-3 h-8 w-8 opacity-40" />
                No child jobs recorded for this run.
              </div>
            ) : (
              <div className="overflow-x-auto">
                <Table className="min-w-[560px]">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Service</TableHead>
                      <TableHead className="hidden sm:table-cell">
                        Engine
                      </TableHead>
                      <TableHead>State</TableHead>
                      <TableHead className="hidden md:table-cell">
                        Started
                      </TableHead>
                      <TableHead className="hidden lg:table-cell">
                        Duration
                      </TableHead>
                      <TableHead className="hidden sm:table-cell">
                        Size
                      </TableHead>
                      <TableHead className="hidden xl:table-cell">
                        Error
                      </TableHead>
                      <TableHead className="w-10" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {jobs.map((job) => {
                      const jobDurationMs =
                        job.started_at && job.finished_at
                          ? new Date(job.finished_at).getTime() -
                            new Date(job.started_at).getTime()
                          : null

                      const detailUrl = `/backups/s3-sources/${job.s3_source_id}/backups/${job.backup_uuid}`

                      return (
                        <TableRow
                          key={job.backup_id}
                          className="hover:bg-muted/50"
                        >
                          <TableCell className="font-medium">
                            <Link
                              to={detailUrl}
                              className="block min-w-0 max-w-[200px] truncate hover:underline sm:max-w-none"
                              title={job.service_name}
                            >
                              {job.service_name}
                            </Link>
                            {/* Mobile-only inline meta so users see something
                                useful below the service name when the columns
                                are hidden. */}
                            <div className="mt-0.5 flex flex-wrap gap-x-2 gap-y-0.5 text-xs text-muted-foreground sm:hidden">
                              <span className="font-mono">{job.engine}</span>
                              {job.size_bytes != null && (
                                <>
                                  <span>·</span>
                                  <span>{formatBytes(job.size_bytes)}</span>
                                </>
                              )}
                            </div>
                          </TableCell>
                          <TableCell className="hidden font-mono text-xs text-muted-foreground sm:table-cell">
                            {job.engine}
                          </TableCell>
                          <TableCell>
                            <Badge variant={stateBadgeVariant(job.state)}>
                              {job.state === 'running' ||
                              job.state === 'pending' ? (
                                <span className="flex items-center gap-1">
                                  <Loader2 className="h-3 w-3 animate-spin" />
                                  {job.state}
                                </span>
                              ) : (
                                job.state
                              )}
                            </Badge>
                          </TableCell>
                          <TableCell className="hidden font-mono text-xs text-muted-foreground md:table-cell">
                            {format(
                              new Date(job.started_at),
                              'MMM d HH:mm:ss',
                            )}
                          </TableCell>
                          <TableCell className="hidden text-sm text-muted-foreground lg:table-cell">
                            {jobDurationMs !== null
                              ? formatDuration(jobDurationMs)
                              : job.state === 'running' ||
                                  job.state === 'pending'
                                ? '…'
                                : '—'}
                          </TableCell>
                          <TableCell className="hidden text-sm sm:table-cell">
                            {job.size_bytes != null
                              ? formatBytes(job.size_bytes)
                              : '—'}
                          </TableCell>
                          <TableCell className="hidden max-w-[280px] xl:table-cell">
                            {job.error_message ? (
                              <Tooltip>
                                <TooltipTrigger asChild>
                                  <span className="block truncate text-xs text-destructive">
                                    {job.error_message}
                                  </span>
                                </TooltipTrigger>
                                <TooltipContent
                                  side="top"
                                  className="max-w-md whitespace-pre-wrap break-words"
                                >
                                  {job.error_message}
                                </TooltipContent>
                              </Tooltip>
                            ) : (
                              <span className="text-xs text-muted-foreground">
                                —
                              </span>
                            )}
                          </TableCell>
                          <TableCell className="w-10 p-0 align-middle">
                            {/* Kebab menu — only when the job is still
                                live. Terminal jobs have no actionable verbs
                                today, so the column shows blank rather
                                than a disabled menu. */}
                            {(job.state === 'running' ||
                              job.state === 'pending') && (
                              <DropdownMenu>
                                <DropdownMenuTrigger asChild>
                                  <Button
                                    variant="ghost"
                                    size="icon"
                                    className="h-8 w-8"
                                    aria-label={`Actions for ${job.service_name}`}
                                  >
                                    <MoreHorizontal className="h-4 w-4" />
                                  </Button>
                                </DropdownMenuTrigger>
                                <DropdownMenuContent align="end">
                                  <DropdownMenuItem
                                    onSelect={(e) => {
                                      // Prevent the menu from auto-closing
                                      // before the AlertDialog mounts.
                                      e.preventDefault()
                                      setJobToCancel({
                                        backup_id: job.backup_id,
                                        service_name: job.service_name,
                                      })
                                    }}
                                    className="text-destructive focus:text-destructive"
                                  >
                                    <Ban className="mr-2 h-4 w-4" />
                                    Cancel
                                  </DropdownMenuItem>
                                </DropdownMenuContent>
                              </DropdownMenu>
                            )}
                          </TableCell>
                        </TableRow>
                      )
                    })}
                  </TableBody>
                </Table>
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      {/* Cancel-the-whole-run confirmation. Open via the header button. */}
      <AlertDialog
        open={showCancelRunDialog}
        onOpenChange={(open) => {
          // Don't allow the dialog to close while the mutation is in flight;
          // we want the spinner to stay visible until the request settles.
          if (!cancelRunMutation.isPending) {
            setShowCancelRunDialog(open)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Cancel this run?</AlertDialogTitle>
            <AlertDialogDescription>
              Every pending and running backup in this scheduler tick will be
              flipped to <strong>failed</strong>. Engines that are mid-dump
              will stop cleanly on the next heartbeat tick (within ~5
              seconds). Already-completed backups stay completed.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={cancelRunMutation.isPending}>
              Keep running
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                cancelRunMutation.mutate()
              }}
              disabled={cancelRunMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {cancelRunMutation.isPending ? (
                <span className="flex items-center gap-2">
                  <Loader2 className="h-4 w-4 animate-spin" />
                  Cancelling…
                </span>
              ) : (
                'Cancel run'
              )}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Cancel-one-job confirmation. Open via the row kebab menu. */}
      <AlertDialog
        open={jobToCancel !== null}
        onOpenChange={(open) => {
          if (!cancelJobMutation.isPending && !open) {
            setJobToCancel(null)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Cancel backup for {jobToCancel?.service_name}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              The backup will be flipped to <strong>failed</strong>. If the
              engine is mid-dump it stops on the next heartbeat tick (~5
              seconds). Other backups in this run keep going.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={cancelJobMutation.isPending}>
              Keep running
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                if (jobToCancel) {
                  cancelJobMutation.mutate(jobToCancel.backup_id)
                }
              }}
              disabled={cancelJobMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {cancelJobMutation.isPending ? (
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
    </TooltipProvider>
  )
}

/** Single labelled stat in the summary grid. */
function SummaryStat({
  label,
  value,
  mono = false,
}: {
  label: string
  value: React.ReactNode
  mono?: boolean
}) {
  return (
    <div className="min-w-0 rounded-lg border bg-card px-3 py-2.5">
      <div className="text-sm text-muted-foreground sm:text-xs">{label}</div>
      <div
        className={`mt-0.5 truncate text-base sm:text-sm ${mono ? 'font-mono' : 'font-medium'}`}
        title={typeof value === 'string' ? value : undefined}
      >
        {value}
      </div>
    </div>
  )
}
