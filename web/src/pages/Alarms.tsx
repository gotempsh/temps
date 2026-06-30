import {
  acknowledgeAlarmMutation,
  getProjectAlarmsSummaryOptions,
  getProjectAlarmsSummaryQueryKey,
  getProjectsOptions,
  listProjectAlarmsOptions,
  listProjectAlarmsQueryKey,
  resolveAlarmMutation,
} from '@/api/client/@tanstack/react-query.gen'
import type { AlarmResponse } from '@/api/client/types.gen'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { EmptyState } from '@/components/ui/empty-state'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
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
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format, formatDistanceToNow } from 'date-fns'
import { AlarmClock, Check, CheckCircle2, X } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'

const PAGE_SIZE = 20
const ALL = '__all__'

// ── Display helpers (no IFEs in JSX) ──────────────────────────────────────

function humanizeType(alarmType: string): string {
  return alarmType
    .split('_')
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ')
}

function severityBadge(severity: string) {
  switch (severity) {
    case 'critical':
      return <Badge variant="destructive">Critical</Badge>
    case 'warning':
      return (
        <Badge className="border-transparent bg-amber-500/15 text-amber-600 hover:bg-amber-500/25 dark:text-amber-400">
          Warning
        </Badge>
      )
    default:
      return <Badge variant="outline">Info</Badge>
  }
}

function statusBadge(status: string) {
  switch (status) {
    case 'firing':
      return (
        <Badge className="border-transparent bg-destructive/10 text-destructive hover:bg-destructive/20">
          <span className="mr-1 inline-block size-1.5 rounded-full bg-destructive" />
          Firing
        </Badge>
      )
    case 'acknowledged':
      return <Badge variant="secondary">Acknowledged</Badge>
    default:
      return (
        <Badge variant="outline" className="text-muted-foreground">
          Resolved
        </Badge>
      )
  }
}

function scopeLabel(alarm: AlarmResponse): string {
  const parts: string[] = []
  if (alarm.environment_id != null) parts.push(`env #${alarm.environment_id}`)
  if (alarm.deployment_id != null) parts.push(`deploy #${alarm.deployment_id}`)
  if (alarm.service_id != null) parts.push(`service #${alarm.service_id}`)
  if (alarm.container_id != null) parts.push(`container #${alarm.container_id}`)
  return parts.length > 0 ? parts.join(' · ') : 'project-wide'
}

// ── Page ────────────────────────────────────────────────────────────────────

export function Alarms() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const queryClient = useQueryClient()
  usePageTitle('Alarms')

  useEffect(() => {
    setBreadcrumbs([{ label: 'Monitoring & Alerts' }, { label: 'Alarms' }])
  }, [setBreadcrumbs])

  const [selectedProjectId, setSelectedProjectId] = useState<number | null>(null)
  const [status, setStatus] = useState<string>(ALL)
  const [severity, setSeverity] = useState<string>(ALL)
  const [alarmType, setAlarmType] = useState<string>(ALL)
  const [page, setPage] = useState(1)

  const { data: projectsData, isLoading: projectsLoading } = useQuery(
    getProjectsOptions({ query: { per_page: 100 } }),
  )
  const projects = projectsData?.projects ?? []

  // Default to the first project once the list loads.
  useEffect(() => {
    if (selectedProjectId == null && projects.length > 0) {
      setSelectedProjectId(projects[0].id)
    }
  }, [projects, selectedProjectId])

  // Reset to the first page whenever filters or the project change.
  useEffect(() => {
    setPage(1)
  }, [selectedProjectId, status, severity, alarmType])

  const hasProject = selectedProjectId != null
  const projectPath = { project_id: selectedProjectId ?? 0 }

  const { data: summary } = useQuery({
    ...getProjectAlarmsSummaryOptions({ path: projectPath }),
    enabled: hasProject,
    refetchInterval: 30_000,
  })

  const { data, isLoading: alarmsLoading } = useQuery({
    ...listProjectAlarmsOptions({
      path: projectPath,
      query: {
        page,
        page_size: PAGE_SIZE,
        status: status === ALL ? undefined : status,
        severity: severity === ALL ? undefined : severity,
        alarm_type: alarmType === ALL ? undefined : alarmType,
      },
    }),
    enabled: hasProject,
    refetchInterval: 30_000,
  })

  // Available type filters derived from what actually fired (summary.by_type).
  const typeOptions = useMemo(() => {
    return Object.keys(summary?.by_type ?? {}).sort()
  }, [summary])

  const invalidate = () => {
    // Built from `path` only (no `query`) so partial matching invalidates
    // every page/filter variant for this project, not just the current one.
    queryClient.invalidateQueries({
      queryKey: listProjectAlarmsQueryKey({ path: projectPath }),
    })
    queryClient.invalidateQueries({
      queryKey: getProjectAlarmsSummaryQueryKey({ path: projectPath }),
    })
  }

  const acknowledge = useMutation({
    ...acknowledgeAlarmMutation(),
    onSuccess: () => {
      toast.success('Alarm acknowledged')
      invalidate()
    },
    onError: (err: Error) => toast.error(`Failed to acknowledge: ${err.message}`),
  })

  const resolve = useMutation({
    ...resolveAlarmMutation(),
    onSuccess: () => {
      toast.success('Alarm resolved')
      invalidate()
    },
    onError: (err: Error) => toast.error(`Failed to resolve: ${err.message}`),
  })

  const items = data?.items ?? []
  const total = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))
  const hasFilters = status !== ALL || severity !== ALL || alarmType !== ALL
  const isMutating = acknowledge.isPending || resolve.isPending

  const resetFilters = () => {
    setStatus(ALL)
    setSeverity(ALL)
    setAlarmType(ALL)
  }

  const summaryCards = [
    { label: 'Active', value: summary?.total_active ?? 0, tone: '' },
    { label: 'Firing', value: summary?.firing ?? 0, tone: 'text-destructive' },
    {
      label: 'Acknowledged',
      value: summary?.acknowledged ?? 0,
      tone: 'text-muted-foreground',
    },
    {
      label: 'Critical',
      value: summary?.critical ?? 0,
      tone: 'text-destructive',
    },
    {
      label: 'Warning',
      value: summary?.warning ?? 0,
      tone: 'text-amber-600 dark:text-amber-400',
    },
  ]

  return (
    <div className="space-y-4">
      {/* Header */}
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1">
          <h1 className="text-2xl font-semibold tracking-tight">Alarms</h1>
          <p className="text-sm text-muted-foreground">
            Firing history across metrics, containers, uptime, and databases —
            acknowledge or resolve from one place.
          </p>
        </div>
        <Select
          value={selectedProjectId != null ? String(selectedProjectId) : undefined}
          onValueChange={(v) => setSelectedProjectId(Number(v))}
          disabled={projectsLoading || projects.length === 0}
        >
          <SelectTrigger className="w-full sm:w-[240px]">
            <SelectValue placeholder="Select a project…" />
          </SelectTrigger>
          <SelectContent>
            {projects.map((p) => (
              <SelectItem key={p.id} value={String(p.id)}>
                {p.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {/* Summary cards */}
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
        {summaryCards.map((c) => (
          <Card key={c.label}>
            <CardContent className="p-3">
              <p className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                {c.label}
              </p>
              <p className={`mt-1 text-2xl font-semibold tabular-nums ${c.tone}`}>
                {c.value}
              </p>
            </CardContent>
          </Card>
        ))}
      </div>

      {/* Filter bar */}
      <Card>
        <CardContent className="p-3">
          <div className="flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
            <Select value={status} onValueChange={setStatus}>
              <SelectTrigger className="w-full sm:w-[180px]">
                <SelectValue placeholder="Status" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>All statuses</SelectItem>
                <SelectItem value="firing">Firing</SelectItem>
                <SelectItem value="acknowledged">Acknowledged</SelectItem>
                <SelectItem value="resolved">Resolved</SelectItem>
              </SelectContent>
            </Select>

            <Select value={severity} onValueChange={setSeverity}>
              <SelectTrigger className="w-full sm:w-[180px]">
                <SelectValue placeholder="Severity" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>All severities</SelectItem>
                <SelectItem value="critical">Critical</SelectItem>
                <SelectItem value="warning">Warning</SelectItem>
                <SelectItem value="info">Info</SelectItem>
              </SelectContent>
            </Select>

            <Select
              value={alarmType}
              onValueChange={setAlarmType}
              disabled={typeOptions.length === 0}
            >
              <SelectTrigger className="w-full sm:w-[200px]">
                <SelectValue placeholder="Type" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={ALL}>All types</SelectItem>
                {typeOptions.map((t) => (
                  <SelectItem key={t} value={t}>
                    {humanizeType(t)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>

            {hasFilters && (
              <Button
                variant="ghost"
                size="sm"
                onClick={resetFilters}
                className="ml-auto"
              >
                <X className="mr-1 h-4 w-4" />
                Clear
              </Button>
            )}
          </div>
        </CardContent>
      </Card>

      {/* Table */}
      <Card>
        <div className="overflow-x-auto">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-[110px]">Severity</TableHead>
                <TableHead>Alarm</TableHead>
                <TableHead className="hidden lg:table-cell">Scope</TableHead>
                <TableHead className="w-[130px]">Status</TableHead>
                <TableHead className="hidden text-right md:table-cell">
                  Fired
                </TableHead>
                <TableHead className="w-[150px] text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {!hasProject && !projectsLoading ? (
                <TableRow className="hover:bg-transparent">
                  <TableCell colSpan={6} className="p-0">
                    <EmptyState
                      icon={AlarmClock}
                      title="No project selected"
                      description="Select a project to view its alarm history."
                    />
                  </TableCell>
                </TableRow>
              ) : alarmsLoading || (projectsLoading && !hasProject) ? (
                Array.from({ length: 6 }).map((_, i) => (
                  <TableRow key={i}>
                    <TableCell>
                      <Skeleton className="h-5 w-16" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="h-4 w-56" />
                    </TableCell>
                    <TableCell className="hidden lg:table-cell">
                      <Skeleton className="h-4 w-28" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="h-5 w-20" />
                    </TableCell>
                    <TableCell className="hidden md:table-cell">
                      <Skeleton className="ml-auto h-4 w-20" />
                    </TableCell>
                    <TableCell>
                      <Skeleton className="ml-auto h-8 w-24" />
                    </TableCell>
                  </TableRow>
                ))
              ) : items.length === 0 ? (
                <TableRow className="hover:bg-transparent">
                  <TableCell colSpan={6} className="p-0">
                    <EmptyState
                      icon={AlarmClock}
                      title="No alarms found"
                      description={
                        hasFilters
                          ? 'Try adjusting your filters to see more results.'
                          : 'Nothing has fired for this project yet.'
                      }
                    />
                  </TableCell>
                </TableRow>
              ) : (
                items.map((alarm) => (
                  <TableRow key={alarm.id}>
                    <TableCell>{severityBadge(alarm.severity)}</TableCell>
                    <TableCell>
                      <div className="font-medium">{alarm.title}</div>
                      {alarm.message && (
                        <div className="text-xs text-muted-foreground line-clamp-1">
                          {alarm.message}
                        </div>
                      )}
                      <div className="mt-0.5 font-mono text-[11px] text-muted-foreground">
                        {humanizeType(alarm.alarm_type)}
                      </div>
                    </TableCell>
                    <TableCell className="hidden text-xs text-muted-foreground lg:table-cell">
                      {scopeLabel(alarm)}
                    </TableCell>
                    <TableCell>{statusBadge(alarm.status)}</TableCell>
                    <TableCell
                      className="hidden text-right text-xs text-muted-foreground md:table-cell"
                      title={format(new Date(alarm.fired_at), 'PPpp')}
                    >
                      {formatDistanceToNow(new Date(alarm.fired_at), {
                        addSuffix: true,
                      })}
                    </TableCell>
                    <TableCell className="text-right">
                      <div className="flex justify-end gap-1">
                        {alarm.status === 'firing' && (
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={isMutating}
                            onClick={() =>
                              acknowledge.mutate({
                                path: {
                                  project_id: selectedProjectId ?? 0,
                                  alarm_id: alarm.id,
                                },
                              })
                            }
                          >
                            <Check className="mr-1 h-3.5 w-3.5" />
                            Ack
                          </Button>
                        )}
                        {alarm.status !== 'resolved' && (
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={isMutating}
                            onClick={() =>
                              resolve.mutate({
                                path: {
                                  project_id: selectedProjectId ?? 0,
                                  alarm_id: alarm.id,
                                },
                              })
                            }
                          >
                            <CheckCircle2 className="mr-1 h-3.5 w-3.5" />
                            Resolve
                          </Button>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </div>
      </Card>

      {/* Pagination */}
      {hasProject && items.length > 0 && (
        <div className="flex items-center justify-between">
          <p className="text-sm text-muted-foreground">
            <span className="hidden sm:inline">
              {total} alarm{total === 1 ? '' : 's'} ·{' '}
            </span>
            Page {page} / {totalPages}
          </p>
          <div className="flex gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              disabled={page === 1 || alarmsLoading}
            >
              Previous
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPage((p) => p + 1)}
              disabled={page >= totalPages || alarmsLoading}
            >
              Next
            </Button>
          </div>
        </div>
      )}
    </div>
  )
}
