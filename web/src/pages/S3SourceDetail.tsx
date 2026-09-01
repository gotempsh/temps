// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import {
  deleteBackupScheduleMutation,
  disableBackupScheduleMutation,
  enableBackupScheduleMutation,
  getS3SourceOptions,
  listBackupSchedulesOptions,
  listSourceBackupsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { BackupScheduleResponse } from '@/api/client/types.gen'
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
import { EmptyState } from '@/components/ui/empty-state'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { listSourceBackupsWithScan, testS3SourceConnection } from '@/lib/s3-sources'
import { runScheduleNow } from '@/lib/schedule-runs'
import { cn, formatBytes, isPitrCapableFormat } from '@/lib/utils'
import { iconForServiceType, serviceTypeRouteForEngine } from '@/lib/serviceIcons'
import { ServiceLogo } from '@/components/ui/service-logo'
import { Input } from '@/components/ui/input'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  ArrowLeft,
  CalendarDays,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Database,
  DatabaseBackup,
  HardDrive,
  Loader2,
  MoreHorizontal,
  Pencil,
  Play,
  Plug,
  Plus,
  Radio,
  ScanSearch,
  Search,
  XCircle,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'

/** Format a wall-clock timeout from seconds into a human-readable string.
 * Mirrors the helper in `BackupDetail.tsx`; if this gets copied a third
 * time it should move to `lib/utils.ts`. */
function formatTimeoutSecs(secs: number): string {
  if (secs <= 0) return '—'
  const hours = Math.floor(secs / 3600)
  const minutes = Math.floor((secs % 3600) / 60)
  if (hours > 0 && minutes > 0) return `${hours}h ${minutes}m`
  if (hours > 0) return `${hours}h`
  return `${minutes}m`
}


export function S3SourceDetail() {
  const { id } = useParams<{ id: string }>()
  const sourceId = id ? parseInt(id) : undefined
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()

  const [scheduleToDelete, setScheduleToDelete] =
    useState<BackupScheduleResponse | null>(null)

  const { data: source, isLoading: isLoadingSource } = useQuery({
    ...getS3SourceOptions({
      path: { id: sourceId! },
    }),
    enabled: !!sourceId,
  })

  const { data: backupIndex, isLoading: isLoadingBackups } = useQuery({
    ...listSourceBackupsOptions({
      path: { id: sourceId! },
    }),
    enabled: !!sourceId,
  })

  const {
    data: schedules = [],
    refetch: refetchSchedules,
    isLoading: isLoadingSchedules,
  } = useQuery({
    ...listBackupSchedulesOptions(),
  })

  const scheduledForSource = useMemo(
    () => schedules.filter((s) => s.s3_source_id === sourceId),
    [schedules, sourceId],
  )

  // Probes the S3 source's credentials, region, and bucket reachability
  // via the existing POST /backups/s3-sources/{id}/test endpoint.
  // Surfaces the server's `{ ok, message }` verbatim in a toast so operators
  // can confirm a source works before relying on it for backups.
  const testConnectionMutation = useMutation({
    mutationFn: () => {
      if (!sourceId) {
        return Promise.reject(new Error('S3 source id is unknown'))
      }
      return testS3SourceConnection(sourceId)
    },
    onSuccess: (result) => {
      if (result.ok) {
        toast.success('S3 connection succeeded', { description: result.message })
      } else {
        toast.error('S3 connection failed', { description: result.message })
      }
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('S3 connection test failed', { description: message })
    },
  })

  const deleteMutation = useMutation({
    ...deleteBackupScheduleMutation(),
    meta: { errorTitle: 'Failed to delete backup schedule' },
    onSuccess: () => {
      refetchSchedules()
      toast.success('Backup schedule deleted successfully')
    },
    onSettled: () => {
      setScheduleToDelete(null)
    },
  })

  const queryClient = useQueryClient()

  // "Discover orphan backups" — explicit opt-in S3 scan. Slow on some
  // endpoints (5-30 s on OVH); only triggered by user action.
  const discoverOrphansMutation = useMutation({
    mutationFn: () => {
      if (!sourceId) {
        return Promise.reject(new Error('S3 source id is unknown'))
      }
      return listSourceBackupsWithScan(sourceId)
    },
    onSuccess: (result) => {
      const scanned = result.backups.filter(
        (b) => (b as { source?: string }).source === 's3_scan',
      ).length
      toast.success(
        scanned > 0
          ? `Found ${scanned} additional backup${scanned === 1 ? '' : 's'} in S3`
          : 'No additional backups found in S3',
      )
      // Refresh the normal listing so newly discovered entries appear.
      void queryClient.invalidateQueries({
        queryKey: ['backups', 's3-source', sourceId],
      })
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('S3 scan failed', { description: message })
    },
  })

  const disableMutation = useMutation({
    ...disableBackupScheduleMutation(),
    meta: { errorTitle: 'Failed to disable backup schedule' },
    onSuccess: () => {
      refetchSchedules()
      toast.success('Backup schedule disabled')
    },
  })

  const enableMutation = useMutation({
    ...enableBackupScheduleMutation(),
    meta: { errorTitle: 'Failed to enable backup schedule' },
    onSuccess: () => {
      refetchSchedules()
      toast.success('Backup schedule enabled')
    },
  })

  const runNowMutation = useMutation({
    mutationFn: (scheduleId: number) => runScheduleNow(scheduleId),
    onSuccess: (_data, scheduleId) => {
      toast.success('Backup run enqueued')
      void navigate(`/backups/schedules/${scheduleId}`)
    },
    onError: (err: unknown) => {
      const message = err instanceof Error ? err.message : 'Unknown error'
      toast.error('Failed to start backup run', { description: message })
    },
  })

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Backups', href: '/backups' },
      { label: source?.name || 'S3 Source Details' },
    ])
  }, [setBreadcrumbs, source?.name])

  usePageTitle(source?.name || 'S3 Source Details')

  const sortedBackups = useMemo(
    () =>
      [...(backupIndex?.backups || [])].sort(
        (a, b) =>
          new Date(b.created_at).getTime() - new Date(a.created_at).getTime(),
      ),
    [backupIndex],
  )

  // Client-side filter + pagination over the recent-backups list. The list
  // is DB-first (ADR-014 §"Fast listings") and capped, so doing this on the
  // client is fine — no backend pagination needed yet. If/when the cap goes
  // away, swap this for `?page=&page_size=` on the API.
  const [backupSearch, setBackupSearch] = useState('')
  const [backupPage, setBackupPage] = useState(1)
  const BACKUP_PAGE_SIZE = 10

  const filteredBackups = useMemo(() => {
    const q = backupSearch.trim().toLowerCase()
    if (!q) return sortedBackups
    return sortedBackups.filter((b) => {
      // Match on the user-visible fields so a user can find a backup by
      // service name, engine, state, format, or any prefix of the UUID.
      return (
        b.name.toLowerCase().includes(q) ||
        (b.origin_service_name ?? '').toLowerCase().includes(q) ||
        (b.engine ?? '').toLowerCase().includes(q) ||
        b.state.toLowerCase().includes(q) ||
        b.backup_id.toLowerCase().includes(q) ||
        (b.format ?? '').toLowerCase().includes(q)
      )
    })
  }, [sortedBackups, backupSearch])

  const backupTotalPages = Math.max(
    1,
    Math.ceil(filteredBackups.length / BACKUP_PAGE_SIZE),
  )
  // Clamp the page if the filter just shrunk the result set below the
  // current page.
  useEffect(() => {
    if (backupPage > backupTotalPages) setBackupPage(backupTotalPages)
  }, [backupPage, backupTotalPages])

  const pagedBackups = useMemo(() => {
    const start = (backupPage - 1) * BACKUP_PAGE_SIZE
    return filteredBackups.slice(start, start + BACKUP_PAGE_SIZE)
  }, [filteredBackups, backupPage])

  const backupRangeStart =
    filteredBackups.length === 0 ? 0 : (backupPage - 1) * BACKUP_PAGE_SIZE + 1
  const backupRangeEnd = Math.min(
    backupPage * BACKUP_PAGE_SIZE,
    filteredBackups.length,
  )

  const handleToggleSchedule = (schedule: BackupScheduleResponse) => {
    if (schedule.enabled) {
      disableMutation.mutate({ path: { id: schedule.id } })
    } else {
      enableMutation.mutate({ path: { id: schedule.id } })
    }
  }

  const confirmDeleteSchedule = () => {
    if (!scheduleToDelete) return
    deleteMutation.mutate({ path: { id: scheduleToDelete.id } })
  }

  if (isLoadingSource) {
    return (
      <div className="flex items-center justify-center py-6">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
      </div>
    )
  }

  if (!source) {
    return (
      <div className="flex flex-col items-center justify-center py-6">
        <h2 className="text-lg font-semibold">S3 Source Not Found</h2>
        <p className="text-sm text-muted-foreground">
          The requested S3 source could not be found.
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

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <Button
            variant="ghost"
            size="icon"
            className="-ml-1 shrink-0 sm:hidden"
            asChild
            aria-label="Back"
          >
            <Link to="/backups">
              <ArrowLeft className="h-4 w-4" />
            </Link>
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="hidden shrink-0 sm:inline-flex"
            asChild
          >
            <Link to="/backups">
              <ArrowLeft className="mr-2 h-4 w-4" />
              Back
            </Link>
          </Button>
        </div>
        <Button
          variant="outline"
          size="sm"
          className="shrink-0"
          onClick={() => testConnectionMutation.mutate()}
          disabled={testConnectionMutation.isPending || !sourceId}
          aria-label="Test connection"
          title="Test connection"
        >
          {testConnectionMutation.isPending ? (
            <Loader2 className="h-4 w-4 animate-spin sm:mr-2" />
          ) : (
            <Plug className="h-4 w-4 sm:mr-2" />
          )}
          <span className="hidden sm:inline">Test connection</span>
        </Button>
      </div>

      <div className="grid gap-6">
        <Card>
          <CardHeader>
            <CardTitle className="flex min-w-0 items-center gap-2">
              <Database className="h-5 w-5 shrink-0" />
              <span className="min-w-0 break-words">{source.name}</span>
            </CardTitle>
            <CardDescription>S3 Storage Configuration</CardDescription>
          </CardHeader>
          <CardContent>
            <dl className="grid gap-4">
              <div className="min-w-0">
                <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                  Bucket Name
                </dt>
                <dd className="break-all text-base sm:text-sm">
                  {source.bucket_name}
                </dd>
              </div>
              <div className="min-w-0">
                <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                  Region
                </dt>
                <dd className="text-base sm:text-sm">{source.region}</dd>
              </div>
              {source.endpoint && (
                <div className="min-w-0">
                  <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                    Endpoint URL
                  </dt>
                  <dd className="break-all text-base sm:text-sm">
                    {source.endpoint}
                  </dd>
                </div>
              )}
              <div className="min-w-0">
                <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                  Force Path Style
                </dt>
                <dd className="text-base sm:text-sm">
                  <Badge
                    variant={source.force_path_style ? 'default' : 'secondary'}
                  >
                    {source.force_path_style ? 'Enabled' : 'Disabled'}
                  </Badge>
                </dd>
              </div>
              <div className="min-w-0">
                <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                  Access Key ID
                </dt>
                <dd className="truncate font-mono text-base sm:text-sm">
                  &bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;
                </dd>
              </div>
              <div className="min-w-0">
                <dt className="text-base font-medium text-muted-foreground sm:text-sm">
                  Secret Key
                </dt>
                <dd className="truncate font-mono text-base sm:text-sm">
                  &bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;&bull;
                </dd>
              </div>
            </dl>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-start justify-between gap-3 space-y-0">
            <div className="min-w-0">
              <CardTitle className="flex items-center gap-2">
                <CalendarDays className="h-5 w-5 shrink-0" />
                Backup Schedules
              </CardTitle>
              <CardDescription>
                Scheduled backups writing to this S3 source
              </CardDescription>
            </div>
            <Button
              size="sm"
              className="shrink-0"
              asChild
              aria-label="New schedule"
              title="New schedule"
            >
              <Link to={`/backups/s3-sources/${sourceId}/schedules/new`}>
                <Plus className="h-4 w-4 sm:mr-2" />
                <span className="hidden sm:inline">New Schedule</span>
              </Link>
            </Button>
          </CardHeader>
          <CardContent>
            {isLoadingSchedules ? (
              <div className="flex items-center justify-center py-6">
                <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-primary"></div>
              </div>
            ) : scheduledForSource.length === 0 ? (
              <EmptyState
                icon={CalendarDays}
                title="No schedules for this source"
                description="Create a schedule to back up automatically to this S3 target"
                action={
                  <Button asChild>
                    <Link
                      to={`/backups/s3-sources/${sourceId}/schedules/new`}
                    >
                      <Plus className="mr-2 h-4 w-4" />
                      New Schedule
                    </Link>
                  </Button>
                }
              />
            ) : (
              <div className="-mx-6 overflow-x-auto">
                <Table className="min-w-[640px]">
                  <TableHeader>
                    <TableRow>
                      <TableHead>Name</TableHead>
                      <TableHead className="hidden sm:table-cell">
                        Type
                      </TableHead>
                      <TableHead>Schedule</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead className="hidden md:table-cell">
                        Retention
                      </TableHead>
                      <TableHead className="hidden lg:table-cell">
                        Timeout
                      </TableHead>
                      <TableHead className="hidden md:table-cell">
                        Last Run
                      </TableHead>
                      <TableHead className="hidden lg:table-cell">
                        Next Run
                      </TableHead>
                      <TableHead className="w-12 text-right">
                        <span className="sr-only">Actions</span>
                      </TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {scheduledForSource.map((schedule) => (
                      <TableRow
                        key={schedule.id}
                        className={cn(
                          'transition-colors hover:bg-accent/50',
                          !schedule.enabled && 'text-muted-foreground',
                        )}
                      >
                        <TableCell className="max-w-[200px] sm:max-w-[280px]">
                          {/* Anchor — supports ⌘-click / middle-click /
                              right-click "Open in new tab". The whole-row
                              click fallback got replaced because <a>
                              inside <tr> with onClick eats modifier-key
                              navigation. */}
                          <Link
                            to={`/backups/schedules/${schedule.id}`}
                            className="flex min-w-0 items-center gap-3 hover:underline"
                          >
                            <DatabaseBackup
                              className={cn(
                                'h-4 w-4 shrink-0',
                                !schedule.enabled &&
                                  'text-muted-foreground/60',
                              )}
                            />
                            <div className="min-w-0">
                              <div className="truncate font-medium">
                                {schedule.name}
                              </div>
                              {schedule.description && (
                                <div
                                  className={cn(
                                    'truncate text-base sm:text-sm',
                                    !schedule.enabled
                                      ? 'text-muted-foreground/60'
                                      : 'text-muted-foreground',
                                  )}
                                  title={schedule.description}
                                >
                                  {schedule.description}
                                </div>
                              )}
                              {/* Mobile-only inline meta so users see Type
                                  even when the column is hidden. */}
                              <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-muted-foreground sm:hidden">
                                <Badge
                                  variant="outline"
                                  className="font-normal"
                                >
                                  {schedule.backup_type}
                                </Badge>
                              </div>
                            </div>
                          </Link>
                        </TableCell>
                        <TableCell className="hidden sm:table-cell">
                          <Badge variant="outline">
                            {schedule.backup_type}
                          </Badge>
                        </TableCell>
                        <TableCell className="whitespace-nowrap font-mono text-xs">
                          {schedule.schedule_expression}
                        </TableCell>
                        <TableCell>
                          <Badge
                            variant={
                              schedule.enabled ? 'default' : 'secondary'
                            }
                          >
                            {schedule.enabled ? 'Enabled' : 'Disabled'}
                          </Badge>
                        </TableCell>
                        <TableCell className="hidden whitespace-nowrap md:table-cell">
                          {schedule.retention_period} days
                        </TableCell>
                        <TableCell className="hidden whitespace-nowrap text-muted-foreground lg:table-cell">
                          {schedule.max_runtime_secs
                            ? formatTimeoutSecs(schedule.max_runtime_secs)
                            : 'engine default'}
                        </TableCell>
                        <TableCell className="hidden whitespace-nowrap md:table-cell">
                          {schedule.last_run
                            ? format(
                                new Date(schedule.last_run),
                                'MMM d, yyyy HH:mm',
                              )
                            : '-'}
                        </TableCell>
                        <TableCell className="hidden whitespace-nowrap lg:table-cell">
                          {schedule.next_run
                            ? format(
                                new Date(schedule.next_run),
                                'MMM d, yyyy HH:mm',
                              )
                            : '-'}
                        </TableCell>
                        <TableCell
                          className="w-12 p-0 pr-2 text-right align-middle"
                          onClick={(e) => e.stopPropagation()}
                        >
                          <DropdownMenu>
                            <DropdownMenuTrigger asChild>
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-8 w-8 shrink-0"
                                aria-label={`Actions for ${schedule.name}`}
                              >
                                <MoreHorizontal className="h-4 w-4" />
                              </Button>
                            </DropdownMenuTrigger>
                            <DropdownMenuContent align="end">
                              <DropdownMenuItem
                                onClick={() =>
                                  runNowMutation.mutate(schedule.id)
                                }
                                disabled={
                                  !schedule.enabled ||
                                  runNowMutation.isPending
                                }
                              >
                                <Play className="mr-2 h-4 w-4" />
                                Run now
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem asChild>
                                <Link
                                  to={`/backups/s3-sources/${sourceId}/schedules/${schedule.id}/edit`}
                                >
                                  <Pencil className="mr-2 h-4 w-4" />
                                  Edit
                                </Link>
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem
                                onClick={() =>
                                  handleToggleSchedule(schedule)
                                }
                                disabled={
                                  disableMutation.isPending ||
                                  enableMutation.isPending
                                }
                              >
                                {schedule.enabled ? 'Disable' : 'Enable'}
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem
                                onClick={() => setScheduleToDelete(schedule)}
                                className="text-destructive"
                                disabled={deleteMutation.isPending}
                              >
                                Delete
                              </DropdownMenuItem>
                            </DropdownMenuContent>
                          </DropdownMenu>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex flex-row items-start justify-between gap-3 space-y-0">
            <div>
              <CardTitle>Recent Backups</CardTitle>
              <CardDescription>
                Backups that have been written to this S3 source
              </CardDescription>
            </div>
            <Button
              variant="outline"
              size="sm"
              onClick={() => discoverOrphansMutation.mutate()}
              disabled={discoverOrphansMutation.isPending || !sourceId}
              title="Scan S3 for backups not tracked in the database — useful after a Temps restore from a different instance"
            >
              {discoverOrphansMutation.isPending ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <ScanSearch className="mr-2 h-4 w-4" />
              )}
              <span className="hidden sm:inline">Discover orphan backups</span>
              <span className="sm:hidden">Scan S3</span>
            </Button>
          </CardHeader>
          <CardContent>
            {/* Search bar — instant client filter across name, service,
                engine, state, UUID, format. */}
            {!isLoadingBackups && sortedBackups.length > 0 && (
              <div className="relative mb-4">
                <Search className="pointer-events-none absolute top-1/2 left-3 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  type="search"
                  placeholder="Search by service, engine, state, or UUID…"
                  value={backupSearch}
                  onChange={(e) => {
                    setBackupSearch(e.target.value)
                    setBackupPage(1)
                  }}
                  className="pl-9"
                />
              </div>
            )}

            {isLoadingBackups ? (
              <div className="space-y-2">
                {[...Array(5)].map((_, i) => (
                  <div
                    key={i}
                    className="h-16 animate-pulse rounded-lg border bg-muted/40"
                  />
                ))}
              </div>
            ) : sortedBackups.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No backups found for this S3 source.
              </p>
            ) : filteredBackups.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No backups match &ldquo;{backupSearch}&rdquo;.
              </p>
            ) : (
              <div className="space-y-2">
                {pagedBackups.map((backup) => {
                  // The same `state` strings the BackupDetail page uses.
                  const state = backup.state || 'unknown'
                  const isCompleted = state === 'completed'
                  const isFailed = state === 'failed'
                  const isRunning = state === 'running' || state === 'pending'
                  const isOrphan = backup.source === 's3_scan'

                  // Prefer the friendly service name; fall back to the
                  // engine label so users always see *something* useful.
                  const displayName =
                    backup.origin_service_name ||
                    backup.engine ||
                    backup.name ||
                    'Backup'

                  // Per-row icon. Prefer the real brand logo
                  // (`<ServiceLogo>`) when the engine maps to a known
                  // service type — Postgres elephant, Redis cube, etc. —
                  // so backups visually match the storage-list rows. Fall
                  // back to the generic Lucide icon (`ServerCog` for the
                  // control plane, `Database` for unknown engines) in a
                  // muted square so something is always shown.
                  const brandService = serviceTypeRouteForEngine(backup.engine)
                  const FallbackIcon = iconForServiceType(backup.engine)

                  return (
                    <Link
                      key={`${backup.source}-${backup.backup_id || backup.location}`}
                      to={`/backups/s3-sources/${id}/backups/${backup.backup_id}`}
                      className="block focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 rounded-lg"
                    >
                      <div className="flex flex-col gap-3 rounded-lg border p-3 transition-colors hover:bg-muted/50 sm:flex-row sm:items-center sm:justify-between sm:p-4">
                        {/* Left: name + meta */}
                        <div className="flex min-w-0 items-center gap-3">
                          {brandService ? (
                            <ServiceLogo
                              service={brandService}
                              className="size-8 shrink-0"
                            />
                          ) : (
                            <div
                              className="flex size-8 shrink-0 items-center justify-center rounded-md border bg-muted/40 text-muted-foreground"
                              aria-hidden
                            >
                              <FallbackIcon className="h-4 w-4" />
                            </div>
                          )}
                          <div className="min-w-0 flex-1">
                            <div className="flex flex-wrap items-center gap-2">
                              <span className="truncate font-medium">
                                {displayName}
                              </span>
                              {backup.engine ? (
                                <Badge
                                  variant="outline"
                                  className="font-mono text-xs"
                                >
                                  {backup.engine}
                                </Badge>
                              ) : null}
                              {isPitrCapableFormat(backup.format) ? (
                                <Badge variant="secondary" className="text-xs">
                                  PITR
                                </Badge>
                              ) : null}
                              {isOrphan ? (
                                <Badge variant="outline" className="text-xs">
                                  orphan
                                </Badge>
                              ) : null}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
                              <span className="font-mono">
                                {format(
                                  new Date(backup.created_at),
                                  'MMM d, yyyy p',
                                )}
                              </span>
                              {backup.size_bytes != null &&
                              backup.size_bytes > 0 ? (
                                <span className="inline-flex items-center gap-1">
                                  <HardDrive className="h-3 w-3" />
                                  {formatBytes(backup.size_bytes)}
                                </span>
                              ) : null}
                              {backup.backup_id ? (
                                <span className="font-mono">
                                  #{backup.backup_id.slice(0, 8)}
                                </span>
                              ) : null}
                            </div>
                          </div>
                        </div>

                        {/* Right: status badge. Same vocabulary as the
                            BackupDetail StatusBadge so users can scan both
                            pages with the same mental model. */}
                        <div className="flex items-center gap-2 sm:shrink-0">
                          {isCompleted ? (
                            <Badge
                              variant="outline"
                              className="border-emerald-500/50 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400 gap-1"
                            >
                              <CheckCircle2 className="h-3 w-3" />
                              Completed
                            </Badge>
                          ) : isFailed ? (
                            <Badge variant="destructive" className="gap-1">
                              <XCircle className="h-3 w-3" />
                              Failed
                            </Badge>
                          ) : isRunning ? (
                            <Badge variant="secondary" className="gap-1">
                              <Radio className="h-3 w-3 animate-pulse" />
                              {state === 'pending' ? 'Pending' : 'Running'}
                            </Badge>
                          ) : (
                            <Badge variant="outline" className="text-xs">
                              {state}
                            </Badge>
                          )}
                        </div>
                      </div>
                    </Link>
                  )
                })}
              </div>
            )}

            {/* Pagination footer — only when more than one page. */}
            {filteredBackups.length > BACKUP_PAGE_SIZE && (
              <div className="mt-4 flex flex-col items-center justify-between gap-2 border-t pt-4 sm:flex-row">
                <span className="hidden text-xs text-muted-foreground sm:inline">
                  Showing {backupRangeStart}–{backupRangeEnd} of{' '}
                  {filteredBackups.length}
                </span>
                <span className="text-xs text-muted-foreground sm:hidden">
                  {backupPage} / {backupTotalPages}
                </span>
                <div className="flex items-center gap-2">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setBackupPage((p) => Math.max(1, p - 1))}
                    disabled={backupPage === 1}
                  >
                    <ChevronLeft className="h-4 w-4" />
                    <span className="hidden sm:inline">Previous</span>
                  </Button>
                  <span className="hidden text-xs text-muted-foreground sm:inline">
                    Page {backupPage} of {backupTotalPages}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() =>
                      setBackupPage((p) => Math.min(backupTotalPages, p + 1))
                    }
                    disabled={backupPage === backupTotalPages}
                  >
                    <span className="hidden sm:inline">Next</span>
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      <AlertDialog
        open={scheduleToDelete !== null}
        onOpenChange={(open) => {
          if (!open && !deleteMutation.isPending) {
            setScheduleToDelete(null)
          }
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete backup schedule?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete the schedule
              {scheduleToDelete ? (
                <>
                  {' '}
                  <span className="font-medium text-foreground">
                    {scheduleToDelete.name}
                  </span>
                </>
              ) : null}
              . Existing backups created by this schedule will not be removed,
              but no new backups will run on this schedule. This action cannot
              be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel disabled={deleteMutation.isPending}>
              Cancel
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={(e) => {
                e.preventDefault()
                confirmDeleteSchedule()
              }}
              disabled={deleteMutation.isPending}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? 'Deleting...' : 'Delete schedule'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
