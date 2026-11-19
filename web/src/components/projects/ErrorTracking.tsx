// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEnvironmentFilter } from '@/components/project-frame/use-environment-filter'
import { PageHeader } from '@/components/layout/PageContainer'
import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getErrorDashboardStatsOptions,
  getErrorDashboardStatsQueryKey,
  getErrorTimeSeriesQueryKey,
  getProjectDeploymentsQueryKey,
  getOrCreateDsnMutation,
  hasErrorGroupsOptions,
  hasErrorGroupsQueryKey,
  listErrorGroupsOptions,
  listErrorGroupsQueryKey,
  listDsnsOptions,
  bulkUpdateErrorGroupsMutation,
  mergeErrorGroupsMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { ErrorTimeSeriesChart } from '@/components/error-tracking/ErrorTimeSeriesChart'
import { AnalyticsFilters } from '@/components/project/ProjectAnalytics'
import { queryKeyPrefix } from '@/lib/refresh'
import { useProjectTourActive } from '@/components/project/ProjectTour'
import { useAuth } from '@/contexts/AuthContext'
import { SavedSearchMenu } from '@/components/error-tracking/SavedSearchMenu'
import type { GroupStatusView } from '@/components/error-tracking/saved-search-view'
import { Input } from '@/components/ui/input'
import { useDebounce } from '@/hooks/useDebounce'
import { useAdvancingDateRange } from '@/hooks/useAdvancingDateRange'
import {
  QUICK_FILTERS,
  type AnalyticsDateFilter,
  type QuickFilter,
} from '@/hooks/useAnalyticsDateRange'
import { AssigneeMenu } from '@/components/error-tracking/AssigneeMenu'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { FixErrorWithAgentButton } from '@/components/ai/FixErrorWithAgentButton'
import { offersFixInList } from '@/components/ai/fix-error-with-agent'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { ConfirmDialog } from '@/components/ui/confirm-dialog'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { EmptyState } from '@/components/ui/empty-state'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { format } from 'date-fns'
import {
  AlertTriangle,
  CheckCircle2,
  ChevronRight,
  EyeOff,
  GitCommitHorizontal,
  Info,
  Settings,
  TrendingDown,
  TrendingUp,
  Users,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { toast } from 'sonner'
import { TimeAgo } from '../utils/TimeAgo'
import { BulkTriageBar } from './BulkTriageBar'
import type { IgnoreConditions } from '@/components/error-tracking/ignore-presets'
import { errorGroupSortQuery, EVENT_COUNT_SORT } from './error-group-sort'
import { errorRowText } from './error-row-text'
import { useWindowQuery } from '@/hooks/useWindowQuery'
import { usePageInRange } from '@/hooks/usePageInRange'

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
  const [environmentFilter, setEnvironmentFilter] = useEnvironmentFilter()
  const [statusFilter, setStatusFilter] =
    useState<GroupStatusView>('unresolved')
  // "Assigned to me" covers the user's teams too (ADR 074, D1.6). A saved
  // search can also hold `user:<id>` or `team:<id>`.
  const [assigneeFilter, setAssigneeFilter] = useState<string>('any')
  const [groupSort, setGroupSort] = useState('last_seen:desc')
  const [searchText, setSearchText] = useState('')
  const search = useDebounce(searchText.trim(), 300)
  const [page, setPage] = useState(1)
  const pageSize = 25

  // Source maps and SDK setup are tabs of the Errors section; this page
  // shows the issues, with their trend on top.
  const openSetup = () => navigate(`/projects/${project.slug}/errors/setup`)
  const projectPath = { project_id: project.id }

  // Convert the quick-filter/custom-range selection into concrete start/end
  // timestamps. MUST be memoized: this component (unlike e.g. ProjectAnalytics's
  // PagesTab, which only passes the result down to child queries) calls
  // useQuery directly with these values here. Recomputing `new Date()` on
  // every render — including the re-renders a settling useQuery itself
  // triggers — would produce a new start_date/end_date on every render,
  // change the query key, refire the fetch, and loop forever.
  // Memoized by the hook; it moves to now only when the user refreshes.
  const { startDate, endDate, advance } = useAdvancingDateRange(
    dateFilter,
    null
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

  // Bulk triage selection, by group id. Cleared whenever the visible page
  // changes, so a resolve can never land on a group the user cannot see.
  const [selectedGroupIds, setSelectedGroupIds] = useState<number[]>([])
  const pageScope = `${statusFilter}\u0000${assigneeFilter}\u0000${search}\u0000${groupSort}\u0000${environmentFilter}\u0000${dateFilter.quickFilter}\u0000${dateFilter.dateRange?.from?.getTime() ?? ''}\u0000${dateFilter.dateRange?.to?.getTime() ?? ''}`
  const [previousPageScope, setPreviousPageScope] = useState(pageScope)
  if (pageScope !== previousPageScope) {
    setPreviousPageScope(pageScope)
    setPage(1)
    setSelectedGroupIds([])
  }
  const [previousListPage, setPreviousListPage] = useState(page)
  if (page !== previousListPage) {
    setPreviousListPage(page)
    setSelectedGroupIds([])
  }

  // Fetch error groups for the project (only if we have errors)
  const {
    data: errorGroupsResponse,
    isLoading: isLoadingGroups,
    isPlaceholderData: groupsArePlaceholder,
  } = useWindowQuery({
    ...listErrorGroupsOptions({
      path: { project_id: project.id },
      query: {
        page,
        page_size: pageSize,
        status: statusFilter === 'all' ? null : statusFilter,
        assignee: assigneeFilter === 'any' ? null : assigneeFilter,
        search: search || null,
        start_date: timeRange.startTime,
        end_date: timeRange.endTime,
        environment_id: environmentFilter,
        ...errorGroupSortQuery(groupSort),
      },
    }),
    enabled: hasErrors,
  })
  // Refresh keeps the page; a moved window can hold fewer groups.
  usePageInRange(
    page,
    errorGroupsResponse && !groupsArePlaceholder
      ? {
          rows: errorGroupsResponse.data.length,
          totalPages: errorGroupsResponse.pagination.total_pages,
        }
      : undefined,
    setPage
  )

  // Fetch error dashboard statistics (only if we have errors)
  const { data: dashboardStats, isLoading: isLoadingDashboardStats } =
    useWindowQuery({
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
    })

  // One request for the whole selection; the console never loops over the
  // single-group endpoint.
  const bulkMutation = useMutation({
    ...bulkUpdateErrorGroupsMutation(),
    meta: { errorTitle: 'Failed to update the selected error groups' },
    onSuccess: (response) => {
      setSelectedGroupIds([])
      const updated = response.updated
      const verb =
        response.action === 'resolve'
          ? 'resolved'
          : response.action === 'ignore'
            ? 'ignored'
            : 'reopened'
      toast.success(
        updated === 1
          ? `1 error group ${verb}`
          : `${updated.toLocaleString()} error groups ${verb}`,
        response.skipped > 0
          ? {
              description: `${response.skipped.toLocaleString()} already ${verb}.`,
            }
          : undefined
      )
      queryClient.invalidateQueries({
        queryKey: queryKeyPrefix(listErrorGroupsQueryKey, {
          path: projectPath,
        }),
      })
      queryClient.invalidateQueries({
        queryKey: queryKeyPrefix(getErrorDashboardStatsQueryKey, {
          path: projectPath,
        }),
      })
    },
  })

  // The oldest selected group survives a merge: it carries the issue's
  // history, and a stable rule keeps the dialog's promise exact.
  const [mergeTarget, setMergeTarget] = useState<number | null>(null)
  const mergeMutation = useMutation({
    ...mergeErrorGroupsMutation(),
    meta: { errorTitle: 'Failed to merge the selected error groups' },
    onSuccess: (response) => {
      setSelectedGroupIds([])
      setMergeTarget(null)
      toast.success(
        `${response.merged_group_ids.length + 1} error groups merged`,
        {
          description: `Future events land in #${response.target_group_id}.`,
        }
      )
      queryClient.invalidateQueries({
        queryKey: queryKeyPrefix(listErrorGroupsQueryKey, {
          path: projectPath,
        }),
      })
    },
    onError: () => setMergeTarget(null),
  })

  const applyBulkAction = (
    action: 'resolve' | 'unresolve' | 'ignore',
    ignore?: IgnoreConditions,
    inNextRelease?: boolean
  ) =>
    bulkMutation.mutate({
      path: { project_id: project.id },
      body: {
        action,
        group_ids: selectedGroupIds,
        ignore,
        in_next_release: inNextRelease ?? false,
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
  const { data: allDsns, refetch: refetchAllDsns } = useQuery({
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
        return 'text-red-700 dark:text-red-400 bg-red-500/15 border border-red-500/20'
      case 'warning':
        return 'text-yellow-700 dark:text-yellow-400 bg-yellow-500/15 border border-yellow-500/20'
      case 'info':
        return 'text-blue-600 dark:text-blue-400 bg-blue-500/15 border border-blue-500/20'
      default:
        return 'text-red-700 dark:text-red-400 bg-red-500/15 border border-red-500/20'
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

  const visibleGroupIds = (errorGroupsResponse?.data ?? []).map((g) => g.id)
  const trendDelta = dashboardStats?.total_errors_change_percent ?? 0
  const trendUp = trendDelta > 0

  type ErrorGroupRow = NonNullable<
    NonNullable<typeof errorGroupsResponse>['data']
  >[number]

  const renderErrorRow = (group: ErrorGroupRow, idx: number) => {
    const isSettled = group.status === 'resolved' || group.status === 'ignored'
    const text = errorRowText(group)
    const onClick = () => handleErrorGroupClick(group.id.toString())

    const envName =
      group.environment_id != null
        ? environments?.find((e) => e.id === group.environment_id)?.name
        : undefined

    const eventsInWindow = group.events_in_range ?? group.total_count
    const showAllTimeHint =
      group.events_in_range != null &&
      group.events_in_range !== group.total_count

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
        <Checkbox
          className="shrink-0"
          checked={selectedGroupIds.includes(group.id)}
          onClick={(e) => e.stopPropagation()}
          onCheckedChange={(checked) =>
            setSelectedGroupIds((current) =>
              checked === true
                ? [...current, group.id]
                : current.filter((id) => id !== group.id)
            )
          }
          aria-label={`Select ${text.headline}`}
        />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 flex-wrap">
            <Badge
              variant="outline"
              className={cn(
                'font-mono text-[10px] font-medium px-1.5 py-0',
                getSeverityColor(group.error_type || 'error')
              )}
            >
              {text.type}
            </Badge>
            {envName && (
              <Badge
                variant="outline"
                className="capitalize text-[10px] px-1.5 py-0"
              >
                {envName}
              </Badge>
            )}
            {/* Basis 0 keeps the message on the badges' line, truncated, so
                every row is the same height; it only wraps below when the
                row is too narrow to give it 12rem. */}
            <p
              className="min-w-[12rem] flex-1 basis-0 font-medium text-sm leading-snug truncate"
              title={group.title}
            >
              {text.headline}
            </p>
            {group.status === 'resolved' && (
              <span className="flex items-center gap-1 text-xs text-green-700 dark:text-green-500 shrink-0">
                <CheckCircle2 className="h-3 w-3" /> Resolved
              </span>
            )}
            {group.status === 'ignored' && (
              <span className="flex items-center gap-1 text-xs text-yellow-700 dark:text-yellow-500 shrink-0">
                <EyeOff className="h-3 w-3" /> Ignored
              </span>
            )}
          </div>
          {text.detail && (
            <p className="mt-1 text-xs text-muted-foreground truncate">
              {text.detail}
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

        {offersFixInList(group.status) && (
          <FixErrorWithAgentButton
            className="shrink-0"
            error={{
              id: group.id,
              title: group.title,
              error_type: group.error_type,
              environment_name: envName,
              project: {
                id: project.id,
                slug: project.slug,
                name: project.name,
              },
            }}
          />
        )}

        <AssigneeMenu
          variant="avatar"
          projectId={project.id}
          groupId={group.id}
          assignee={group.assignee}
          legacyAssignedTo={group.assigned_to}
          currentUserId={currentUser?.id}
        />

        <ChevronRight className="h-4 w-4 text-muted-foreground/60 group-hover:text-muted-foreground shrink-0" />
      </div>
    )
  }

  return (
    <div className="space-y-5">
      <PageHeader
        title="Errors"
        description={
          hasErrors && dashboardStats ? (
            <span>
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
                  {environments?.find((e) => e.id === environmentFilter)
                    ?.name ?? 'selected environment'}
                  )
                </>
              )}
              {trendDelta !== 0 && (
                <span
                  className={cn(
                    'ml-2 inline-flex items-center gap-0.5 text-xs font-medium tabular-nums',
                    trendUp
                      ? 'text-red-700 dark:text-red-500'
                      : 'text-green-700 dark:text-green-500'
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
            </span>
          ) : (
            'Track exceptions and stack traces from your applications.'
          )
        }
      />

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
          refreshQueryKeys={[
            queryKeyPrefix(hasErrorGroupsQueryKey, { path: projectPath }),
            queryKeyPrefix(listErrorGroupsQueryKey, { path: projectPath }),
            queryKeyPrefix(getErrorDashboardStatsQueryKey, {
              path: projectPath,
            }),
            queryKeyPrefix(getErrorTimeSeriesQueryKey, { path: projectPath }),
            // The chart's deployment markers.
            queryKeyPrefix(getProjectDeploymentsQueryKey, {
              path: { id: project.id },
            }),
          ]}
          onRefresh={advance}
          leftActions={
            <div className="flex flex-wrap gap-2">
              <SavedSearchMenu
                projectId={project.id}
                canModerate={currentUser?.role === 'admin'}
                view={{
                  status: statusFilter,
                  assignee: assigneeFilter,
                  sort: groupSort,
                  environmentId: environmentFilter,
                  search: searchText,
                }}
                onApply={(view) => {
                  setStatusFilter(view.status)
                  setAssigneeFilter(view.assignee)
                  setGroupSort(view.sort)
                  setEnvironmentFilter(view.environmentId)
                  setSearchText(view.search)
                }}
              />
              <Input
                value={searchText}
                onChange={(e) => setSearchText(e.target.value)}
                placeholder="Search title or message"
                aria-label="Search error groups by title, type or message"
                className="h-9 w-[200px]"
                maxLength={256}
              />
              <Select
                value={statusFilter}
                onValueChange={(v) => setStatusFilter(v as GroupStatusView)}
              >
                <SelectTrigger className="w-[130px]">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="unresolved">Unresolved</SelectItem>
                  <SelectItem value="resolved">Resolved</SelectItem>
                  <SelectItem value="ignored">Ignored</SelectItem>
                  <SelectItem value="all">All</SelectItem>
                </SelectContent>
              </Select>
              <Select value={assigneeFilter} onValueChange={setAssigneeFilter}>
                <SelectTrigger
                  className="w-[160px]"
                  aria-label="Filter error groups by assignee"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="any">Anyone</SelectItem>
                  <SelectItem value="me">Assigned to me</SelectItem>
                  <SelectItem value="none">Unassigned</SelectItem>
                  {!['any', 'me', 'none'].includes(assigneeFilter) && (
                    <SelectItem value={assigneeFilter}>
                      {assigneeFilter.startsWith('team:')
                        ? 'A specific team'
                        : 'A specific person'}
                    </SelectItem>
                  )}
                </SelectContent>
              </Select>
              <Select value={groupSort} onValueChange={setGroupSort}>
                <SelectTrigger
                  className="w-[170px]"
                  aria-label="Sort error groups"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="last_seen:desc">Recently seen</SelectItem>
                  <SelectItem value="first_seen:desc">Newest issues</SelectItem>
                  <SelectItem value="first_seen:asc">Oldest issues</SelectItem>
                  <SelectItem value={EVENT_COUNT_SORT.most}>
                    Most events
                  </SelectItem>
                  <SelectItem value={EVENT_COUNT_SORT.fewest}>
                    Fewest events
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
          }
        />
      )}

      {!hasErrors && !isCheckingErrors && (
        <Alert className="border-blue-200 dark:border-blue-900 bg-blue-50/50 dark:bg-blue-950/20">
          <Info className="h-4 w-4 text-blue-600" />
          <AlertDescription className="text-sm">
            No errors have been tracked yet.{' '}
            {hasDsn
              ? 'Your error tracking is configured and ready to receive errors.'
              : 'Get started by setting up your DSN below.'}
          </AlertDescription>
        </Alert>
      )}

      <div className="space-y-3">
        {hasErrors && (
          <ErrorTimeSeriesChart
            project={project}
            startDate={startDate ?? new Date()}
            endDate={endDate ?? new Date()}
            environmentId={environmentFilter}
          />
        )}
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
              <BulkTriageBar
                pageGroupIds={visibleGroupIds}
                selectedGroupIds={selectedGroupIds}
                onSelectionChange={setSelectedGroupIds}
                onAction={applyBulkAction}
                onMerge={() => setMergeTarget(Math.min(...selectedGroupIds))}
                isPending={bulkMutation.isPending || mergeMutation.isPending}
              />
              <ConfirmDialog
                open={mergeTarget !== null}
                onOpenChange={(open) => !open && setMergeTarget(null)}
                title={`Merge ${selectedGroupIds.length} error groups`}
                description={
                  mergeTarget !== null
                    ? `Group #${mergeTarget} survives. The others keep their events and stop being listed, and their future events land in #${mergeTarget}. You can undo this from the surviving group.`
                    : ''
                }
                confirmLabel="Merge"
                destructive={false}
                onConfirm={() => {
                  if (mergeTarget === null) return
                  mergeMutation.mutate({
                    path: { project_id: project.id, group_id: mergeTarget },
                    body: {
                      source_group_ids: selectedGroupIds.filter(
                        (id) => id !== mergeTarget
                      ),
                    },
                  })
                }}
              />
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
              size="compact"
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
            size="compact"
            icon={Info}
            title="No errors detected"
            description="Your application is running smoothly with no errors reported."
            action={
              !hasDsn && (
                <Button onClick={openSetup}>
                  <Settings className="h-4 w-4 mr-2" /> Configure Error Tracking
                </Button>
              )
            }
          />
        )}
      </div>
      {/* end shared Analytics / Source Maps / Setup tabs */}

      {/* Create/Regenerate DSN Dialog */}
      <Dialog open={showCreateDialog} onOpenChange={setShowCreateDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create DSN</DialogTitle>
            <DialogDescription>
              Create a new Data Source Name for error tracking in your service.
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
