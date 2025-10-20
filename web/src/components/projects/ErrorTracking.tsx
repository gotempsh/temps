import { ProjectResponse } from '@/api/client'
import {
  getEnvironmentsOptions,
  getErrorDashboardStatsOptions,
  getOrCreateDsnMutation,
  hasErrorGroupsOptions,
  listErrorGroupsOptions,
  listDsnsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ErrorTimeSeriesChart } from '@/components/error-tracking/ErrorTimeSeriesChart'
import { Alert, AlertDescription } from '@/components/ui/alert'
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
import {
  Activity,
  AlertTriangle,
  Bug,
  ChevronDown,
  ChevronRight,
  Info,
  Plus,
  RefreshCw,
  Settings,
  Shield,
  TrendingDown,
  TrendingUp,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { toast } from 'sonner'
import { TimeAgo } from '../utils/TimeAgo'
import { CopyButton } from '../ui/copy-button'

interface ErrorTrackingProps {
  project: ProjectResponse
}

export function ErrorTracking({ project }: ErrorTrackingProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [searchParams, setSearchParams] = useSearchParams()
  const [selectedTimeRange, setSelectedTimeRange] = useState<
    '1h' | '24h' | '7d' | '30d'
  >('24h')
  const [isDsnConfigOpen, setIsDsnConfigOpen] = useState(false)

  // Get tab from URL or default to 'errors'
  const selectedTab =
    (searchParams.get('tab') as 'errors' | 'analytics' | 'setup') || 'errors'
  const setSelectedTab = (tab: 'errors' | 'analytics' | 'setup') => {
    setSearchParams((prev) => {
      const params = new URLSearchParams(prev)
      params.set('tab', tab)
      return params
    })
  }

  // Convert time range to start/end times - memoized to prevent infinite loops
  const timeRange = useMemo(() => {
    const now = new Date()
    const endTime = now.toISOString()
    const startTime = new Date()

    switch (selectedTimeRange) {
      case '1h':
        startTime.setHours(startTime.getHours() - 1)
        break
      case '24h':
        startTime.setDate(startTime.getDate() - 1)
        break
      case '7d':
        startTime.setDate(startTime.getDate() - 7)
        break
      case '30d':
        startTime.setDate(startTime.getDate() - 30)
        break
    }

    return { startTime: startTime.toISOString(), endTime }
  }, [selectedTimeRange])
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

  // Fetch error groups for the project (only if we have errors)
  const { data: errorGroupsResponse, isLoading: isLoadingGroups } = useQuery({
    ...listErrorGroupsOptions({
      path: { project_id: project.id },
      query: {
        page_size: 50,
        start_date: timeRange.startTime,
        end_date: timeRange.endTime,
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
        },
      }),
      enabled: hasErrors,
    }
  )

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
        return 'text-red-600 bg-red-100 dark:bg-red-900/20'
      case 'warning':
        return 'text-yellow-600 bg-yellow-100 dark:bg-yellow-900/20'
      case 'info':
        return 'text-blue-600 bg-blue-100 dark:bg-blue-900/20'
      default:
        return 'text-gray-600 bg-gray-100 dark:bg-gray-900/20'
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

  return (
    <div className="space-y-6">
      {/* Show dashboard only if we have errors */}
      {hasErrors ? (
        <>
          {/* Time Range Selector */}
          <div className="flex items-center justify-between">
            <h2 className="text-lg font-medium">Error Tracking Overview</h2>
            <Select
              value={selectedTimeRange}
              onValueChange={(v) =>
                setSelectedTimeRange(v as '1h' | '24h' | '7d' | '30d')
              }
            >
              <SelectTrigger className="w-[120px]">
                <SelectValue placeholder="Time range" />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="1h">Last hour</SelectItem>
                <SelectItem value="24h">Last 24h</SelectItem>
                <SelectItem value="7d">Last 7 days</SelectItem>
                <SelectItem value="30d">Last 30 days</SelectItem>
              </SelectContent>
            </Select>
          </div>

          {/* Error Dashboard Statistics Cards */}
          {dashboardStats && (
            <div className="grid gap-4 md:grid-cols-3">
              <Card>
                <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
                  <CardTitle className="text-sm font-medium">
                    Total Errors
                  </CardTitle>
                  <Bug className="h-4 w-4 text-muted-foreground" />
                </CardHeader>
                <CardContent>
                  <div className="text-2xl font-bold">
                    {dashboardStats.total_errors.toLocaleString()}
                  </div>
                  <p className="text-xs text-muted-foreground flex items-center gap-1">
                    {dashboardStats.total_errors_change_percent !== 0 && (
                      <>
                        {dashboardStats.total_errors_change_percent > 0 ? (
                          <TrendingUp className="h-3 w-3 text-red-500" />
                        ) : (
                          <TrendingDown className="h-3 w-3 text-green-500" />
                        )}
                        <span
                          className={
                            dashboardStats.total_errors_change_percent > 0
                              ? 'text-red-600'
                              : 'text-green-600'
                          }
                        >
                          {Math.abs(
                            dashboardStats.total_errors_change_percent
                          ).toFixed(1)}
                          %
                        </span>
                      </>
                    )}
                    {dashboardStats.total_errors_change_percent === 0
                      ? 'No change'
                      : 'from last period'}
                  </p>
                </CardContent>
              </Card>

              <Card>
                <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
                  <CardTitle className="text-sm font-medium">
                    Error Groups
                  </CardTitle>
                  <AlertTriangle className="h-4 w-4 text-muted-foreground" />
                </CardHeader>
                <CardContent>
                  <div className="text-2xl font-bold">
                    {dashboardStats.error_groups.toLocaleString()}
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {dashboardStats.error_groups_previous_period > 0 ? (
                      <>
                        {dashboardStats.error_groups >
                        dashboardStats.error_groups_previous_period
                          ? '+'
                          : ''}
                        {dashboardStats.error_groups -
                          dashboardStats.error_groups_previous_period}{' '}
                        from last period
                      </>
                    ) : (
                      'Unique error signatures'
                    )}
                  </p>
                </CardContent>
              </Card>

              <Card>
                <CardHeader className="flex flex-row items-center justify-between space-y-0 pb-2">
                  <CardTitle className="text-sm font-medium">
                    Previous Period
                  </CardTitle>
                  <Activity className="h-4 w-4 text-muted-foreground" />
                </CardHeader>
                <CardContent>
                  <div className="text-2xl font-bold">
                    {dashboardStats.total_errors_previous_period.toLocaleString()}
                  </div>
                  <p className="text-xs text-muted-foreground">
                    {dashboardStats.error_groups_previous_period.toLocaleString()}{' '}
                    error groups
                  </p>
                </CardContent>
              </Card>
            </div>
          )}
        </>
      ) : (
        /* Show setup instructions when no errors */
        !isCheckingErrors && (
          <Alert className="border-blue-200 bg-blue-50/50 dark:bg-blue-950/20">
            <Info className="h-4 w-4 text-blue-600" />
            <AlertDescription className="text-sm">
              No errors have been tracked yet.{' '}
              {hasDsn
                ? 'Your error tracking is configured and ready to receive errors.'
                : 'Get started by setting up your DSN below.'}
            </AlertDescription>
          </Alert>
        )
      )}

      {/* Tabs */}
      <Tabs
        value={selectedTab}
        onValueChange={(v) =>
          setSelectedTab(v as 'errors' | 'analytics' | 'setup')
        }
      >
        <TabsList className="grid w-full grid-cols-3 max-w-[600px]">
          <TabsTrigger value="errors">
            Error Groups
            {hasErrors && (
              <Badge variant="secondary" className="ml-2">
                {errorGroupsResponse?.pagination?.total_count}
              </Badge>
            )}
          </TabsTrigger>
          <TabsTrigger value="analytics">Analytics</TabsTrigger>
          <TabsTrigger value="setup">
            DSN & Setup
            {!hasDsn && (
              <Badge variant="outline" className="ml-2 text-yellow-600">
                !
              </Badge>
            )}
          </TabsTrigger>
        </TabsList>

        {/* Errors Tab */}
        <TabsContent value="errors" className="mt-6">
          <Card>
            <CardContent className="pt-6 space-y-4">
              {hasErrors ? (
                <>
                  {/* Time Range Filter */}
                  <div className="flex justify-end">
                    <div className="flex gap-2">
                      {['1h', '24h', '7d', '30d'].map((range) => (
                        <Button
                          key={range}
                          variant={
                            selectedTimeRange === range ? 'default' : 'outline'
                          }
                          size="sm"
                          onClick={() =>
                            setSelectedTimeRange(
                              range as typeof selectedTimeRange
                            )
                          }
                        >
                          {range}
                        </Button>
                      ))}
                    </div>
                  </div>

                  {/* Error Groups List */}
                  {isLoadingGroups ? (
                    <div className="space-y-4">
                      {[...Array(3)].map((_, i) => (
                        <Skeleton key={i} className="h-24" />
                      ))}
                    </div>
                  ) : errorGroupsResponse?.pagination?.total_count &&
                    errorGroupsResponse?.pagination?.total_count > 0 ? (
                    <div className="space-y-4">
                      {errorGroupsResponse?.data?.map((group) => (
                        <div
                          key={group.id}
                          className="flex items-start space-x-4 rounded-lg border p-4 hover:bg-muted/50 cursor-pointer transition-colors"
                          onClick={() =>
                            handleErrorGroupClick(group.id.toString())
                          }
                        >
                          <div className="flex-1 space-y-2">
                            <div className="flex items-start justify-between">
                              <div className="space-y-1">
                                <div className="flex items-center gap-2">
                                  <Badge
                                    className={cn(
                                      getSeverityColor(
                                        group.error_type || 'error'
                                      )
                                    )}
                                  >
                                    {group.error_type || 'error'}
                                  </Badge>
                                  <span className="font-medium">
                                    {group.title}
                                  </span>
                                </div>
                                {group.message_template && (
                                  <p className="text-sm text-muted-foreground line-clamp-2">
                                    {group.message_template}
                                  </p>
                                )}
                              </div>
                              <ChevronRight className="h-5 w-5 text-muted-foreground" />
                            </div>
                            <div className="flex items-center gap-4 text-sm text-muted-foreground">
                              <span className="flex items-center gap-1">
                                <Bug className="h-3 w-3" />
                                {group.total_count} occurrences
                              </span>
                              <span className="flex items-center gap-1">
                                <Activity className="h-3 w-3" />
                                {group.status}
                              </span>
                              {group.first_seen && (
                                <span>
                                  First seen <TimeAgo date={group.first_seen} />
                                </span>
                              )}
                              {group.last_seen && (
                                <span>
                                  Last seen <TimeAgo date={group.last_seen} />
                                </span>
                              )}
                            </div>
                          </div>
                        </div>
                      ))}
                    </div>
                  ) : (
                    <EmptyState
                      icon={AlertTriangle}
                      title="No errors in this period"
                      description={`No error groups found in the last ${selectedTimeRange === '1h' ? 'hour' : selectedTimeRange === '24h' ? '24 hours' : selectedTimeRange === '7d' ? '7 days' : '30 days'}. Try selecting a different time range or check back later.`}
                    />
                  )}
                </>
              ) : (
                <EmptyState
                  icon={Info}
                  title="No errors detected"
                  description="Your application is running smoothly with no errors reported."
                  action={
                    !hasDsn && (
                      <Button onClick={() => setSelectedTab('setup')}>
                        <Settings className="h-4 w-4 mr-2" />
                        Configure Error Tracking
                      </Button>
                    )
                  }
                />
              )}
            </CardContent>
          </Card>
        </TabsContent>

        {/* Analytics Tab */}
        <TabsContent value="analytics" className="mt-6">
          <ErrorTimeSeriesChart
            project={project}
            startDate={new Date(timeRange.startTime)}
            endDate={new Date(timeRange.endTime)}
          />
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
                  <CollapsibleTrigger asChild>
                    <Button
                      variant="ghost"
                      className="w-full justify-between p-0 hover:bg-transparent"
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
