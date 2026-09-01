// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getErrorDashboardStatsOptions,
  getOrCreateDsnMutation,
  hasErrorGroupsOptions,
  listErrorGroupsOptions,
  listDsnsOptions,
  listUsersOptions,
  updateErrorGroupMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ErrorTimeSeriesChart } from '@/components/error-tracking/ErrorTimeSeriesChart'
import { AnalyticsFilters } from '@/components/project/ProjectAnalytics'
import { useProjectTourActive } from '@/components/project/ProjectTour'
import { useAuth } from '@/contexts/AuthContext'
import {
  getDateRangeFromFilter,
  QUICK_FILTERS,
  type AnalyticsDateFilter,
  type QuickFilter,
} from '@/hooks/useAnalyticsDateRange'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { CodeBlock } from '@/components/ui/code-block'
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from '@/components/ui/collapsible'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { EmptyState } from '@/components/ui/empty-state'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  EyeOff,
  GitCommitHorizontal,
  Info,
  Plus,
  RefreshCw,
  Settings,
  Shield,
  TrendingDown,
  TrendingUp,
  UserRound,
  Users,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { TimeAgo } from '../utils/TimeAgo'
import { CopyButton } from '../ui/copy-button'
import { SourceMaps } from '../error-tracking/SourceMaps'

interface ErrorTrackingProps {
  project: ProjectResponse
}

export function ErrorTracking({ project }: ErrorTrackingProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { user: currentUser } = useAuth()
  const [searchParams, setSearchParams] = useSearchParams()
  // Restore the date filter from URL search params. Also honour the legacy
  // `?range=` deep-link (e.g. from a firing metric's "what changed" strip) so
  // old links still land on roughly the same window.
  const [dateFilter, setDateFilter] = useState<AnalyticsDateFilter>(() => {
    const filter = searchParams.get('filter') as QuickFilter | null
    const from = searchParams.get('from')
    const to = searchParams.get('to')
    if (filter === 'custom' && from && to) {
      return {
        quickFilter: 'custom',
        dateRange: { from: new Date(from), to: new Date(to) },
      }
    }
    if (filter && QUICK_FILTERS.some((f) => f.value === filter)) {
      return { quickFilter: filter, dateRange: undefined }
    }
    const legacyRange = searchParams.get('range')
    const legacyMap: Partial<Record<string, QuickFilter>> = {
      '1h': 'lasthour',
      '6h': '24hours',
      '24h': '24hours',
      '7d': '7days',
      '30d': '30days',
    }
    if (legacyRange && legacyMap[legacyRange]) {
      return { quickFilter: legacyMap[legacyRange]!, dateRange: undefined }
    }
    return { quickFilter: '24hours', dateRange: undefined }
  })
  const updateDateFilter = (next: AnalyticsDateFilter) => {
    setDateFilter(next)
    setSearchParams((prev) => {
      const params = new URLSearchParams(prev)
      params.delete('range')
      params.set('filter', next.quickFilter)
      if (
        next.quickFilter === 'custom' &&
        next.dateRange?.from &&
        next.dateRange?.to
      ) {
        params.set('from', next.dateRange.from.toISOString())
        params.set('to', next.dateRange.to.toISOString())
      } else {
        params.delete('from')
        params.delete('to')
      }
      return params
    })
  }
  const [environmentFilter, setEnvironmentFilter] = useState<
    number | undefined
  >(undefined)
  const [isRefreshing, setIsRefreshing] = useState(false)
  const [isDsnConfigOpen, setIsDsnConfigOpen] = useState(false)
  const [statusFilter, setStatusFilter] = useState<
    'unresolved' | 'resolved' | 'all'
  >('unresolved')
  const [page, setPage] = useState(1)
  const pageSize = 25

  // Get tab from URL or default to 'errors'
  const selectedTab =
    (searchParams.get('tab') as
      'errors' | 'analytics' | 'sourcemaps' | 'setup') || 'errors'
  const setSelectedTab = (
    tab: 'errors' | 'analytics' | 'sourcemaps' | 'setup'
  ) => {
    setSearchParams((prev) => {
      const params = new URLSearchParams(prev)
      params.set('tab', tab)
      return params
    })
  }

  // Convert the quick-filter/custom-range selection into concrete start/end
  // timestamps. MUST be memoized: this component (unlike e.g. ProjectAnalytics's
  // PagesTab, which only passes the result down to child queries) calls
  // useQuery directly with these values here. Recomputing `new Date()` on
  // every render — including the re-renders a settling useQuery itself
  // triggers — would produce a new start_date/end_date on every render,
  // change the query key, refire the fetch, and loop forever.
  const { startDate, endDate } = useMemo(
    () => getDateRangeFromFilter(dateFilter),
    [dateFilter.quickFilter, dateFilter.dateRange?.from, dateFilter.dateRange?.to]
  )
  const timeRange = {
    startTime: (startDate ?? new Date()).toISOString(),
    endTime: (endDate ?? new Date()).toISOString(),
  }
  const activeFilterMeta = QUICK_FILTERS.find(
    (f) => f.value === dateFilter.quickFilter
  )
  const rangeLabel =
    dateFilter.quickFilter === 'custom' && startDate && endDate
      ? `${format(startDate, 'MMM d, HH:mm')} – ${format(endDate, 'MMM d, HH:mm')}`
      : (activeFilterMeta?.label.toLowerCase() ?? 'selected period')
  const [showCreateDialog, setShowCreateDialog] = useState(false)
  const [dialogEnvironmentId, setDialogEnvironmentId] = useState<string>('')

  // Fetch project environments
  const { data: environments, isLoading: isLoadingEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  // Instance users, used to resolve `assigned_to` (an email/username string)
  // to a display name + avatar for the assignee control. No per-project
  // members endpoint exists yet, so this mirrors the same source
  // TeamDetail's AddMemberDialog uses for its user picker.
  const { data: usersData } = useQuery(
    listUsersOptions({ query: { include_deleted: false } })
  )
  const userByIdentity = useMemo(() => {
    const map = new Map<string, NonNullable<typeof usersData>[number]>()
    for (const u of usersData ?? []) {
      if (u.user.email) map.set(u.user.email.toLowerCase(), u)
      if (u.user.username) map.set(u.user.username.toLowerCase(), u)
    }
    return map
  }, [usersData])

  // Derive selected environment from environments data
  const selectedEnvironmentId = useMemo(() => {
    if (!environments || environments.length === 0) return undefined
    const productionEnv = environments.find(
      (env) => env.name.toLowerCase() === 'production'
    )
    return productionEnv ? productionEnv.id : environments[0].id
  }, [environments])

  // Check if project has any error groups
  const { data: hasErrorGroupsData, isLoading: isCheckingErrors } = useQuery({
    ...hasErrorGroupsOptions({
      path: { project_id: project.id },
    }),
  })

  // Determine if we have errors
  const hasErrors = hasErrorGroupsData?.has_error_groups || false

  // Reset to page 1 whenever filters change
  useEffect(() => {
    setPage(1)
  }, [
    statusFilter,
    environmentFilter,
    dateFilter.quickFilter,
    dateFilter.dateRange?.from,
    dateFilter.dateRange?.to,
  ])

  // Fetch error groups for the project (only if we have errors)
  const { data: errorGroupsResponse, isLoading: isLoadingGroups } = useQuery({
    ...listErrorGroupsOptions({
      path: { project_id: project.id },
      query: {
        page,
        page_size: pageSize,
        status: statusFilter === 'all' ? null : statusFilter,
        start_date: timeRange.startTime,
        end_date: timeRange.endTime,
        environment_id: environmentFilter,
      },
    }),
    enabled: hasErrors,
  })

  // Fetch error dashboard statistics (only if we have errors)
  const { data: dashboardStats, isLoading: isLoadingDashboardStats } = useQuery(
    {
      ...getErrorDashboardStatsOptions({
        path: { project_id: project.id },
        query: {
          start_time: timeRange.startTime,
          end_time: timeRange.endTime,
          compare_to_previous: true,
          environment_id: environmentFilter,
        },
      }),
      enabled: hasErrors,
    }
  )

  const refreshableIds = new Set([
    'listErrorGroups',
    'getErrorDashboardStats',
    'getErrorTimeSeries',
  ])
  const handleRefresh = () => {
    setIsRefreshing(true)
    queryClient.invalidateQueries({
      predicate: (query) => {
        const key = query.queryKey[0] as { _id?: string } | undefined
        return !!key?._id && refreshableIds.has(key._id)
      },
    })
    setTimeout(() => setIsRefreshing(false), 800)
  }

  // Shared mutation for the assignee quick-actions on each row ("Assign to
  // me" / "Unassign"). `assigned_to: ''` is the backend's clear-assignment
  // sentinel — see UpdateErrorGroupRequest's doc comment.
  const assigneeMutation = useMutation({
    ...updateErrorGroupMutation(),
    meta: { errorTitle: 'Failed to update assignee' },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: listErrorGroupsOptions({ path: { project_id: project.id } })
          .queryKey,
      })
    },
  })

  // Fetch DSN for the selected environment (always fetch when environment is selected)
  const { data: dsnInfo, refetch: refetchDsn } = useQuery({
    ...listDsnsOptions({
      path: { project_id: project.id },
      // query: { environment_id: parseInt(selectedEnvironmentId) }
    }),
    enabled: !!selectedEnvironmentId,
  })

  // Fetch all DSNs for the project
  const {
    data: allDsns,
    isLoading: isLoadingAllDsns,
    refetch: refetchAllDsns,
  } = useQuery({
    ...listDsnsOptions({
      path: { project_id: project.id },
    }),
  })

  // When the project has never received any errors, route to the onboarding
  // wizard (mirrors analytics empty-state behavior). Skip while the guided
  // tour is showing off this page — it should render its own empty state
  // rather than get bounced to /setup out from under the tour.
  const isTourActive = useProjectTourActive()
  useEffect(() => {
    if (isCheckingErrors || isTourActive) return
    if (!hasErrorGroupsData?.has_error_groups) {
      navigate(`/projects/${project.slug}/errors/setup`, { replace: true })
    }
  }, [
    isCheckingErrors,
    isTourActive,
    hasErrorGroupsData?.has_error_groups,
    navigate,
    project.slug,
  ])

  // Create DSN mutation
  const createDsnMutation = useMutation({
    ...getOrCreateDsnMutation(),
    meta: {
      errorTitle: 'Failed to create DSN',
    },
    onSuccess: () => {
      const envName =
        environments?.find((e) => e.id.toString() === dialogEnvironmentId)
          ?.name || 'selected'
      toast.success(`DSN created for ${envName} environment`)
      setShowCreateDialog(false)
      setDialogEnvironmentId('') // Reset dialog environment
      queryClient.invalidateQueries({ queryKey: ['getProjectDsn'] })
      queryClient.invalidateQueries({ queryKey: ['listProjectDsns'] })
      refetchDsn()
      refetchAllDsns()
    },
  })

  const handleErrorGroupClick = (groupId: string) => {
    navigate(`/projects/${project.slug}/errors/${groupId}`)
  }

  const getSeverityColor = (level: string) => {
    switch (level?.toLowerCase()) {
      case 'error':
      case 'fatal':
      case 'referenceerror':
      case 'typeerror':
      case 'syntaxerror':
      case 'rangeerror':
        return 'text-red-400 bg-red-500/15 border border-red-500/20'
      case 'warning':
        return 'text-yellow-400 bg-yellow-500/15 border border-yellow-500/20'
      case 'info':
        return 'text-blue-400 bg-blue-500/15 border border-blue-500/20'
      default:
        return 'text-red-400 bg-red-500/15 border border-red-500/20'
    }
  }
  const handleCreateOrRegenerateDsn = () => {
    if (!dialogEnvironmentId) {
      toast.error('Please select an environment')
      return
    }
    createDsnMutation.mutate({
      path: { project_id: project.id },
      body: {
        environment_id: parseInt(dialogEnvironmentId),
      },
    })
  }

  const hasDsn = Boolean(dsnInfo?.[0]?.dsn)

  // Generate AI prompt for coding agents to set up error tracking
  const getErrorTrackingAiPrompt = () => {
    const dsn = allDsns?.[0]?.dsn || 'YOUR_DSN_HERE'
    const envName = allDsns?.[0]
      ? environments?.find((e) => e.id === allDsns[0].environment_id)?.name ||
        'production'
      : 'production'

    return `Add Sentry-compatible error tracking to my application. The error tracking endpoint uses a Sentry-compatible DSN.

## DSN

\`\`\`
${dsn}
\`\`\`

## JavaScript (Browser)

### Install
\`\`\`bash
npm install @sentry/browser
\`\`\`

### Initialize
\`\`\`javascript
import * as Sentry from "@sentry/browser";

Sentry.init({
  dsn: "${dsn}",
  environment: "${envName}",
  integrations: [
    new Sentry.BrowserTracing(),
    new Sentry.Replay(),
  ],
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
});
\`\`\`

## React

### Install
\`\`\`bash
npm install @sentry/react
\`\`\`

### Initialize
\`\`\`javascript
import * as Sentry from "@sentry/react";

Sentry.init({
  dsn: "${dsn}",
  environment: "${envName}",
  integrations: [
    Sentry.replayIntegration(),
  ],
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
});
\`\`\`

## Node.js

### Install
\`\`\`bash
npm install @sentry/node
\`\`\`

### Initialize
\`\`\`javascript
const Sentry = require("@sentry/node");

Sentry.init({
  dsn: "${dsn}",
  environment: "${envName}",
  tracesSampleRate: 1.0,
});
\`\`\`

## Python

### Install
\`\`\`bash
pip install sentry-sdk
\`\`\`

### Initialize
\`\`\`python
import sentry_sdk

sentry_sdk.init(
    dsn="${dsn}",
    environment="${envName}",
    traces_sample_rate=1.0,
    profiles_sample_rate=1.0,
)
\`\`\`

## Verification

After setup, trigger a test error and check the Temps error tracking dashboard to confirm events are arriving.`
  }

  if (
    isCheckingErrors ||
    isLoadingEnvironments ||
    (hasErrors && (isLoadingGroups || isLoadingDashboardStats))
  ) {
    return (
      <div className="space-y-6">
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
          {[...Array(4)].map((_, i) => (
            <Card key={i}>
              <CardHeader className="p-6">
                <Skeleton className="h-4 w-20 mb-2" />
                <Skeleton className="h-8 w-32" />
              </CardHeader>
            </Card>
          ))}
        </div>
        <Card>
          <CardHeader>
            <Skeleton className="h-6 w-32" />
            <Skeleton className="h-4 w-48" />
          </CardHeader>
          <CardContent>
            <div className="space-y-4">
              {[...Array(3)].map((_, i) => (
                <Skeleton key={i} className="h-20" />
              ))}
            </div>
          </CardContent>
        </Card>
      </div>
    )
  }

  const trendDelta = dashboardStats?.total_errors_change_percent ?? 0
  const trendUp = trendDelta > 0

  type ErrorGroupRow = NonNullable<
    NonNullable<typeof errorGroupsResponse>['data']
  >[number]

  const renderErrorRow = (group: ErrorGroupRow, idx: number) => {
    const isSettled = group.status === 'resolved' || group.status === 'ignored'
    const messageDiffers =
      group.message_template && group.message_template !== group.title
    const onClick = () => handleErrorGroupClick(group.id.toString())

    const envName =
      group.environment_id != null
        ? environments?.find((e) => e.id === group.environment_id)?.name
        : undefined

    const eventsInWindow = group.events_in_range ?? group.total_count
    const showAllTimeHint =
      group.events_in_range != null &&
      group.events_in_range !== group.total_count

    const assignedUser = group.assigned_to
      ? userByIdentity.get(group.assigned_to.toLowerCase())
      : undefined
    const assigneeLabel = group.assigned_to
      ? (assignedUser?.user.name ?? group.assigned_to)
      : 'Unassigned'
    const assigneeInitials = (
      assignedUser?.user.username ||
      assignedUser?.user.name ||
      group.assigned_to ||
      '?'
    )
      .slice(0, 2)
      .toUpperCase()
    const isAssignedToMe =
      !!group.assigned_to &&
      !!currentUser?.email &&
      group.assigned_to.toLowerCase() === currentUser.email.toLowerCase()

    const assign = (assignedTo: string) =>
      assigneeMutation.mutate({
        path: { project_id: project.id, group_id: group.id },
        body: { status: group.status ?? 'unresolved', assigned_to: assignedTo },
      })

    return (
      <div
        key={group.id}
        className={cn(
          'group flex items-center gap-4 py-3 cursor-pointer transition-colors hover:bg-muted/40 -mx-3 px-3 rounded-md',
          isSettled && 'opacity-55',
          idx > 0 && 'border-t border-border/60'
        )}
        onClick={onClick}
      >
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <Badge
              variant="outline"
              className={cn(
                'text-[10px] font-medium uppercase tracking-wide px-1.5 py-0',
                getSeverityColor(group.error_type || 'error')
              )}
            >
              {group.error_type || 'error'}
            </Badge>
            {envName && (
              <Badge
                variant="outline"
                className="capitalize text-[10px] px-1.5 py-0"
              >
                {envName}
              </Badge>
            )}
            <p className="font-medium text-sm leading-snug truncate">
              {group.title}
            </p>
            {group.status === 'resolved' && (
              <span className="flex items-center gap-1 text-xs text-green-500 shrink-0">
                <CheckCircle2 className="h-3 w-3" /> Resolved
              </span>
            )}
            {group.status === 'ignored' && (
              <span className="flex items-center gap-1 text-xs text-yellow-500 shrink-0">
                <EyeOff className="h-3 w-3" /> Ignored
              </span>
            )}
          </div>
          {messageDiffers && (
            <p className="mt-1 text-xs text-muted-foreground truncate">
              {group.message_template}
            </p>
          )}
          <div className="mt-1 flex items-center gap-3 text-xs text-muted-foreground">
            {group.last_seen && (
              <span>
                Last <TimeAgo date={group.last_seen} />
              </span>
            )}
            {group.first_seen &&
              group.last_seen &&
              group.first_seen !== group.last_seen && (
                <span className="hidden sm:inline">
                  First <TimeAgo date={group.first_seen} />
                </span>
              )}
            {group.deployment?.commit_hash && (
              <Link
                to={`/projects/${project.slug}/deployments/${group.deployment.id}`}
                onClick={(e) => e.stopPropagation()}
                className="hidden sm:inline-flex items-center gap-1 font-mono text-muted-foreground hover:text-foreground transition-colors"
                title={
                  group.deployment.commit_message?.split('\n')[0] ??
                  group.deployment.commit_hash
                }
              >
                <GitCommitHorizontal className="h-3 w-3" />
                {group.deployment.commit_hash.slice(0, 7)}
              </Link>
            )}
          </div>
        </div>

        {group.affected_users != null && group.affected_users > 0 && (
          <div
            className="hidden sm:flex items-center gap-1 text-xs text-muted-foreground tabular-nums shrink-0"
            title={`${group.affected_users.toLocaleString()} affected user${group.affected_users === 1 ? '' : 's'}`}
          >
            <Users className="h-3.5 w-3.5" />
            {group.affected_users.toLocaleString()}
          </div>
        )}

        <div className="flex flex-col items-end shrink-0 tabular-nums text-right">
          <div className="flex items-baseline gap-1.5">
            <span className="text-base font-semibold leading-none">
              {eventsInWindow.toLocaleString()}
            </span>
            <span className="text-[11px] text-muted-foreground leading-none">
              events
            </span>
          </div>
          {showAllTimeHint && (
            <span className="hidden md:inline text-[10px] text-muted-foreground leading-none mt-1">
              {group.total_count.toLocaleString()} all-time
            </span>
          )}
        </div>

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button
              type="button"
              onClick={(e) => e.stopPropagation()}
              className="shrink-0 rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring hover:ring-2 hover:ring-border transition-shadow"
              aria-label={`Assignee: ${assigneeLabel}`}
              title={assigneeLabel}
            >
              {group.assigned_to ? (
                <Avatar className="h-6 w-6">
                  <AvatarImage src={assignedUser?.user.image} />
                  <AvatarFallback className="text-[10px]">
                    {assigneeInitials}
                  </AvatarFallback>
                </Avatar>
              ) : (
                <span className="flex h-6 w-6 items-center justify-center rounded-full border border-dashed border-muted-foreground/40 text-muted-foreground">
                  <UserRound className="h-3.5 w-3.5" />
                </span>
              )}
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" onClick={(e) => e.stopPropagation()}>
            <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
              {assigneeLabel}
            </DropdownMenuLabel>
            <DropdownMenuSeparator />
            {!isAssignedToMe && currentUser && (
              <DropdownMenuItem
                onClick={() =>
                  assign(currentUser.email || currentUser.username)
                }
              >
                Assign to me
              </DropdownMenuItem>
            )}
            {group.assigned_to && (
              <DropdownMenuItem onClick={() => assign('')}>
                Unassign
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>

        <ChevronRight className="h-4 w-4 text-muted-foreground/60 group-hover:text-muted-foreground shrink-0" />
      </div>
    )
  }

  return (
    <div className="space-y-5">
      <div className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Errors</h2>
        {hasErrors && dashboardStats ? (
          <p className="text-sm text-muted-foreground">
            <span className="font-medium text-foreground tabular-nums">
              {dashboardStats.total_errors.toLocaleString()}
            </span>{' '}
            events across{' '}
            <span className="font-medium text-foreground tabular-nums">
              {dashboardStats.error_groups.toLocaleString()}
            </span>{' '}
            groups in the {rangeLabel}
            {environmentFilter != null && (
              <>
                {' '}
                (
                {environments?.find((e) => e.id === environmentFilter)?.name ??
                  'selected environment'}
                )
              </>
            )}
            {trendDelta !== 0 && (
              <span
                className={cn(
                  'ml-2 inline-flex items-center gap-0.5 text-xs font-medium tabular-nums',
                  trendUp ? 'text-red-500' : 'text-green-500'
                )}
              >
                {trendUp ? (
                  <TrendingUp className="h-3 w-3" />
                ) : (
                  <TrendingDown className="h-3 w-3" />
                )}
                {Math.abs(trendDelta).toFixed(1)}% vs previous
              </span>
            )}
          </p>
        ) : (
          <p className="text-sm text-muted-foreground">
            Track exceptions and stack traces from your applications.
          </p>
        )}
      </div>

      {hasErrors && (
        <AnalyticsFilters
          project={project}
          activeFilter={dateFilter.quickFilter}
          dateRange={dateFilter.dateRange}
          selectedEnvironment={environmentFilter}
          onFilterChange={(filter) =>
            updateDateFilter({ ...dateFilter, quickFilter: filter })
          }
          onDateRangeChange={(range) =>
            updateDateFilter({
              quickFilter: range ? 'custom' : dateFilter.quickFilter,
              dateRange: range,
            })
          }
          onEnvironmentChange={setEnvironmentFilter}
          onRefresh={handleRefresh}
          isRefreshing={isRefreshing}
          leftActions={
            selectedTab === 'errors' ? (
              <Select
                value={statusFilter}
                onValueChange={(v) =>
                  setStatusFilter(v as 'unresolved' | 'resolved' | 'all')
                }
              >
                <SelectTrigger className="w-[130px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="unresolved">Unresolved</SelectItem>
                  <SelectItem value="resolved">Resolved</SelectItem>
                  <SelectItem value="all">All</SelectItem>
                </SelectContent>
              </Select>
            ) : undefined
          }
        />
      )}

      {!hasErrors && !isCheckingErrors && (
        <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
          <Info className="h-4 w-4 text-blue-600" />
          <AlertDescription className="text-sm">
            No errors have been tracked yet.{' '}
            {hasDsn
              ? 'Your error tracking is configured and ready to receive errors.'
              : 'Get started by setting up your DSN below.'}
          </AlertDescription>
        </Alert>
      )}

      <Tabs
        value={selectedTab}
        onValueChange={(v) =>
          setSelectedTab(v as 'errors' | 'analytics' | 'sourcemaps' | 'setup')
        }
      >
        <TabsList className="grid w-full grid-cols-4 max-w-[700px]">
          <TabsTrigger value="errors">
            Error Groups
            {hasErrors && (
              <Badge variant="secondary" className="ml-2">
                {errorGroupsResponse?.pagination?.total_count}
              </Badge>
            )}
          </TabsTrigger>
          <TabsTrigger value="analytics">Analytics</TabsTrigger>
          <TabsTrigger value="sourcemaps">Source Maps</TabsTrigger>
          <TabsTrigger value="setup">
            DSN & Setup
            {!hasDsn && (
              <Badge variant="outline" className="ml-2 text-yellow-600">
                !
              </Badge>
            )}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="errors" className="mt-5 space-y-3">
          {hasErrors ? (
            isLoadingGroups ? (
              <div className="space-y-3">
                {[...Array(5)].map((_, i) => (
                  <Skeleton key={i} className="h-16" />
                ))}
              </div>
            ) : errorGroupsResponse?.pagination?.total_count &&
              errorGroupsResponse.pagination.total_count > 0 ? (
              <>
                <div className="rounded-md border border-border/60 bg-card px-3">
                  {errorGroupsResponse.data?.map((group, idx) =>
                    renderErrorRow(group, idx)
                  )}
                </div>
                {errorGroupsResponse.pagination.total_pages > 1 && (
                  <div className="flex items-center justify-between pt-1">
                    <p className="text-xs text-muted-foreground tabular-nums">
                      <span className="hidden sm:inline">
                        Showing{' '}
                        {(errorGroupsResponse.pagination.page - 1) *
                          errorGroupsResponse.pagination.page_size +
                          1}
                        –
                        {Math.min(
                          errorGroupsResponse.pagination.page *
                            errorGroupsResponse.pagination.page_size,
                          errorGroupsResponse.pagination.total_count
                        )}{' '}
                        of {errorGroupsResponse.pagination.total_count}
                      </span>
                      <span className="sm:hidden">
                        {errorGroupsResponse.pagination.page} /{' '}
                        {errorGroupsResponse.pagination.total_pages}
                      </span>
                    </p>
                    <div className="flex items-center gap-2">
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => setPage((p) => Math.max(1, p - 1))}
                        disabled={errorGroupsResponse.pagination.page <= 1}
                      >
                        Previous
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => setPage((p) => p + 1)}
                        disabled={
                          errorGroupsResponse.pagination.page >=
                          errorGroupsResponse.pagination.total_pages
                        }
                      >
                        Next
                      </Button>
                    </div>
                  </div>
                )}
              </>
            ) : (
              <EmptyState
                icon={AlertTriangle}
                title={
                  statusFilter === 'unresolved'
                    ? 'No unresolved errors'
                    : statusFilter === 'resolved'
                      ? 'No resolved errors'
                      : 'No errors in this period'
                }
                description={
                  statusFilter === 'unresolved'
                    ? `Nothing needs attention in the ${rangeLabel}${environmentFilter != null ? ' for the selected environment' : ''}.`
                    : `No ${statusFilter === 'all' ? '' : statusFilter + ' '}error groups found in the ${rangeLabel}${environmentFilter != null ? ' for the selected environment' : ''}.`
                }
              />
            )
          ) : (
            <EmptyState
              icon={Info}
              title="No errors detected"
              description="Your application is running smoothly with no errors reported."
              action={
                !hasDsn && (
                  <Button onClick={() => setSelectedTab('setup')}>
                    <Settings className="h-4 w-4 mr-2" /> Configure Error
                    Tracking
                  </Button>
                )
              }
            />
          )}
        </TabsContent>

        {/* Analytics Tab */}
        <TabsContent value="analytics" className="mt-6">
          <ErrorTimeSeriesChart
            project={project}
            startDate={startDate ?? new Date()}
            endDate={endDate ?? new Date()}
            environmentId={environmentFilter}
          />
        </TabsContent>

        {/* Source Maps Tab */}
        <TabsContent value="sourcemaps" className="mt-6">
          <SourceMaps project={project} />
        </TabsContent>

        {/* Setup Tab */}
        <TabsContent value="setup" className="mt-6">
          <div className="space-y-6">
            {/* DSN List Card */}
            <Card>
              <CardHeader>
                <div className="flex items-center justify-between">
                  <CardTitle>DSN Configuration</CardTitle>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setDialogEnvironmentId('')
                      setShowCreateDialog(true)
                    }}
                  >
                    <Plus className="h-4 w-4 mr-2" />
                    Create DSN
                  </Button>
                </div>
              </CardHeader>
              <CardContent className="space-y-4">
                {isLoadingAllDsns ? (
                  <div className="space-y-4">
                    {[...Array(2)].map((_, i) => (
                      <Skeleton key={i} className="h-24" />
                    ))}
                  </div>
                ) : allDsns && allDsns.length > 0 ? (
                  <div className="space-y-4">
                    {allDsns.map((dsn) => {
                      const env = environments?.find(
                        (e) => e.id === dsn.environment_id
                      )
                      return (
                        <div
                          key={dsn.id || dsn.environment_id}
                          className="rounded-lg border p-4 space-y-3"
                        >
                          <div className="flex items-center justify-between">
                            <div className="flex items-center gap-2">
                              <Shield className="h-4 w-4 text-muted-foreground" />
                              <Label className="text-base font-semibold">
                                {env?.name || 'Unknown Environment'}
                              </Label>
                            </div>
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() => {
                                setDialogEnvironmentId(
                                  dsn.environment_id?.toString() || ''
                                )
                                setShowCreateDialog(true)
                              }}
                            >
                              <RefreshCw className="h-4 w-4 mr-2" />
                              Regenerate
                            </Button>
                          </div>
                          <div className="space-y-2">
                            <div className="flex gap-2">
                              <Input
                                value={dsn.dsn || ''}
                                readOnly
                                className="font-mono text-sm"
                              />
                              <CopyButton value={dsn.dsn || ''} />
                            </div>
                            <p className="text-xs text-muted-foreground">
                              Use this DSN in your {env?.name?.toLowerCase()}{' '}
                              environment to send errors to this project
                            </p>
                          </div>
                        </div>
                      )
                    })}
                  </div>
                ) : (
                  <Alert>
                    <Info className="h-4 w-4" />
                    <AlertDescription>
                      <strong>No DSNs configured yet.</strong>
                      <br />
                      Click &quot;Create DSN&quot; to generate one and start
                      tracking errors.
                    </AlertDescription>
                  </Alert>
                )}
              </CardContent>
            </Card>

            {/* SDK Setup Instructions - Collapsible */}
            <Collapsible
              open={isDsnConfigOpen}
              onOpenChange={setIsDsnConfigOpen}
            >
              <Card>
                <CardHeader>
                  <div className="flex items-center justify-between gap-2">
                    <CollapsibleTrigger asChild>
                      <Button
                        variant="ghost"
                        className="flex-1 justify-between p-0 hover:bg-transparent"
                      >
                        <CardTitle className="text-base">
                          SDK Setup Instructions
                        </CardTitle>
                        <ChevronDown
                          className={cn(
                            'h-5 w-5 transition-transform',
                            isDsnConfigOpen && 'rotate-180'
                          )}
                        />
                      </Button>
                    </CollapsibleTrigger>
                    <CopyButton
                      value={getErrorTrackingAiPrompt()}
                      className="shrink-0 rounded-md border border-border px-3 py-1.5 text-xs font-medium"
                    >
                      Copy AI Prompt
                    </CopyButton>
                  </div>
                </CardHeader>
                <CollapsibleContent>
                  <CardContent className="space-y-6">
                    <Tabs defaultValue="javascript" className="w-full">
                      <TabsList className="grid w-full grid-cols-4">
                        <TabsTrigger value="javascript">JavaScript</TabsTrigger>
                        <TabsTrigger value="react">React</TabsTrigger>
                        <TabsTrigger value="nodejs">Node.js</TabsTrigger>
                        <TabsTrigger value="python">Python</TabsTrigger>
                      </TabsList>

                      {/* JavaScript */}
                      <TabsContent value="javascript" className="space-y-4">
                        <div className="space-y-2">
                          <Label>1. Install the SDK</Label>
                          <CodeBlock
                            code="npm install @sentry/browser"
                            language="bash"
                          />
                        </div>
                        <div className="space-y-2">
                          <Label>2. Initialize in your app</Label>
                          <CodeBlock
                            code={`import * as Sentry from "@sentry/browser";

Sentry.init({
  dsn: "${allDsns?.[0]?.dsn || 'YOUR_DSN_HERE'}",
  environment: "${allDsns?.[0] ? environments?.find((e) => e.id === allDsns[0].environment_id)?.name || 'production' : 'production'}",
  integrations: [
    new Sentry.BrowserTracing(),
    new Sentry.Replay(),
  ],
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
});`}
                            language="javascript"
                          />
                        </div>
                      </TabsContent>

                      {/* React */}
                      <TabsContent value="react" className="space-y-4">
                        <div className="space-y-2">
                          <Label>1. Install the SDK</Label>
                          <CodeBlock
                            code="npm install @sentry/react"
                            language="bash"
                          />
                        </div>
                        <div className="space-y-2">
                          <Label>2. Initialize in your app</Label>
                          <CodeBlock
                            code={`import * as Sentry from "@sentry/react";

Sentry.init({
  dsn: "${allDsns?.[0]?.dsn || 'YOUR_DSN_HERE'}",
  environment: "${allDsns?.[0] ? environments?.find((e) => e.id === allDsns[0].environment_id)?.name || 'production' : 'production'}",
  integrations: [
    Sentry.replayIntegration(),
  ],
  tracesSampleRate: 1.0,
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0,
});`}
                            language="javascript"
                          />
                        </div>
                      </TabsContent>

                      {/* Node.js */}
                      <TabsContent value="nodejs" className="space-y-4">
                        <div className="space-y-2">
                          <Label>1. Install the SDK</Label>
                          <CodeBlock
                            code="npm install @sentry/node"
                            language="bash"
                          />
                        </div>
                        <div className="space-y-2">
                          <Label>2. Initialize in your app</Label>
                          <CodeBlock
                            code={`const Sentry = require("@sentry/node");

Sentry.init({
  dsn: "${allDsns?.[0]?.dsn || 'YOUR_DSN_HERE'}",
  environment: "${allDsns?.[0] ? environments?.find((e) => e.id === allDsns[0].environment_id)?.name || 'production' : 'production'}",
  tracesSampleRate: 1.0,
});`}
                            language="javascript"
                          />
                        </div>
                      </TabsContent>

                      {/* Python */}
                      <TabsContent value="python" className="space-y-4">
                        <div className="space-y-2">
                          <Label>1. Install the SDK</Label>
                          <CodeBlock
                            code="pip install sentry-sdk"
                            language="bash"
                          />
                        </div>
                        <div className="space-y-2">
                          <Label>2. Initialize in your app</Label>
                          <CodeBlock
                            code={`import sentry_sdk

sentry_sdk.init(
    dsn="${allDsns?.[0]?.dsn || 'YOUR_DSN_HERE'}",
    environment="${allDsns?.[0] ? environments?.find((e) => e.id === allDsns[0].environment_id)?.name || 'production' : 'production'}",
    traces_sample_rate=1.0,
    profiles_sample_rate=1.0,
)`}
                            language="python"
                          />
                        </div>
                      </TabsContent>
                    </Tabs>
                  </CardContent>
                </CollapsibleContent>
              </Card>
            </Collapsible>
          </div>
        </TabsContent>
      </Tabs>
      {/* end shared Analytics / Source Maps / Setup tabs */}

      {/* Create/Regenerate DSN Dialog */}
      <Dialog open={showCreateDialog} onOpenChange={setShowCreateDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create DSN</DialogTitle>
            <DialogDescription>
              Create a new Data Source Name for error tracking in your project.
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="dialog-environment">Environment</Label>
              <Select
                value={dialogEnvironmentId}
                onValueChange={setDialogEnvironmentId}
              >
                <SelectTrigger id="dialog-environment" className="w-full">
                  <SelectValue placeholder="Select environment" />
                </SelectTrigger>
                <SelectContent>
                  {environments?.map((env) => (
                    <SelectItem key={env.id} value={env.id.toString()}>
                      {env.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>

            {/* Check if DSN already exists for selected environment */}
            {allDsns?.some(
              (dsn) => dsn.environment_id?.toString() === dialogEnvironmentId
            ) && (
              <Alert variant="destructive">
                <AlertTriangle className="h-4 w-4" />
                <AlertDescription>
                  <strong>Warning:</strong> A DSN already exists for this
                  environment. Creating a new one will replace the existing DSN.
                </AlertDescription>
              </Alert>
            )}
          </div>

          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowCreateDialog(false)}
            >
              Cancel
            </Button>
            <Button
              variant={
                allDsns?.some(
                  (dsn) =>
                    dsn.environment_id?.toString() === dialogEnvironmentId
                )
                  ? 'destructive'
                  : 'default'
              }
              onClick={handleCreateOrRegenerateDsn}
              disabled={createDsnMutation.isPending || !dialogEnvironmentId}
            >
              {createDsnMutation.isPending
                ? 'Creating...'
                : allDsns?.some(
                      (dsn) =>
                        dsn.environment_id?.toString() === dialogEnvironmentId
                    )
                  ? 'Replace DSN'
                  : 'Create DSN'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
