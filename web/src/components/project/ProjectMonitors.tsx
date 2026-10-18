// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  listMonitorsOptions,
  getProjectRuntimeHealthOptions,
  createMonitorMutation,
  deleteMonitorMutation,
  getBucketedStatusOptions,
  getEnvironmentsOptions,
  getCurrentMonitorStatusOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  ProjectResponse,
  MonitorResponse,
  EnvironmentResponse,
} from '@/api/client'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { Card, CardContent } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { EmptyState } from '@/components/ui/empty-state'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Activity,
  Moon,
  Plus,
  Trash2,
  ExternalLink,
  Globe,
  MoreVertical,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import * as z from 'zod'
import { toast } from 'sonner'
import { formatDistanceToNow, subDays } from 'date-fns'
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { NumberInput } from '@/components/ui/number-input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'

import { RuntimeHealthPanel } from './RuntimeHealthPanel'
import { visibleExternalMonitors } from './runtime-health-display'

interface ProjectMonitorsProps {
  project: ProjectResponse
}

// Validation schema for creating a monitor
const createMonitorSchema = z.object({
  name: z.string().min(1, 'Monitor name is required'),
  monitor_type: z.string().min(1, 'Monitor type is required'),
  environment_id: z.number({
    error: 'Environment is required',
  }),
  check_interval_seconds: z
    .number({ message: 'Check interval is required' })
    .min(30, 'Check interval must be at least 30 seconds'),
})

type CreateMonitorFormData = z.infer<typeof createMonitorSchema>

interface MonitorCardProps {
  monitor: MonitorResponse
  projectSlug: string
  onDelete: () => void
  environment?: EnvironmentResponse
}

function MonitorCard({
  monitor,
  projectSlug,
  onDelete,
  environment,
}: MonitorCardProps) {
  const isOnDemand = environment?.deployment_config?.onDemand === true
  const { startDate, endDate } = useMemo(() => {
    const now = new Date()
    return {
      startDate: subDays(now, 1),
      endDate: now,
    }
  }, [])
  // Fetch status timeline for the monitor
  const { data: statusData } = useQuery({
    ...getBucketedStatusOptions({
      path: {
        monitor_id: monitor.id,
      },
      query: {
        interval: 'hourly',
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
      },
    }),
    refetchInterval: 30000, // Refresh every 30 seconds
  })

  const { data: currentMonitorStatus } = useQuery({
    ...getCurrentMonitorStatusOptions({
      path: {
        monitor_id: monitor.id,
      },
      query: {
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
      },
    }),
    refetchInterval: 30000, // Refresh every 30 seconds to match status timeline
  })

  const uptimePercentage = useMemo(
    () => currentMonitorStatus?.uptime_percentage,
    [currentMonitorStatus]
  )

  return (
    <Card className="hover:bg-accent/50 transition-colors">
      <CardContent className="flex items-center justify-between p-4">
        <div className="flex items-center gap-4 flex-1 min-w-0">
          {/* Icon */}
          <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-md border bg-background">
            <Activity className="h-5 w-5" />
          </div>

          {/* Monitor Info */}
          <div className="flex-1 min-w-0 space-y-1">
            <div className="flex items-center gap-2">
              <Link
                to={`/projects/${projectSlug}/monitors/${monitor.id}`}
                className="font-semibold hover:text-primary transition-colors"
              >
                {monitor.name}
              </Link>
              <Badge
                variant={monitor.is_active ? 'default' : 'secondary'}
                className="shrink-0"
              >
                {monitor.is_active ? 'RUNNING' : 'STOPPED'}
              </Badge>
            </div>
            <div className="flex items-center gap-2 text-sm text-muted-foreground flex-wrap">
              <span className="uppercase">{monitor.monitor_type}</span>
              <span>•</span>
              <span>Check every {monitor.check_interval_seconds}s</span>
              <span>•</span>
              <span>
                Created{' '}
                {formatDistanceToNow(new Date(monitor.created_at), {
                  addSuffix: true,
                })}
              </span>
            </div>
          </div>
        </div>

        {/* Right Side - Status Timeline & Actions */}
        <div className="flex items-center gap-4 shrink-0">
          {/* Status Timeline */}
          <div className="flex flex-col items-end gap-2">
            {isOnDemand ? (
              <>
                <div className="flex items-center gap-2">
                  <div className="text-xs text-muted-foreground flex items-center gap-1">
                    <Moon className="h-3.5 w-3.5" />
                    On-demand {environment?.sleeping ? '· Sleeping' : '· Awake'}
                  </div>
                </div>
                <div className="flex gap-0.5 h-6 w-48">
                  {Array.from({ length: 48 }).map((_, idx) => (
                    <div key={idx} className="flex-1 rounded-sm bg-muted" />
                  ))}
                </div>
              </>
            ) : (
              <>
                <div className="flex items-center gap-2">
                  <div className="text-lg font-semibold">
                    {uptimePercentage?.toFixed(0) ?? 'N/A'}%
                  </div>
                  <div
                    className={`text-xs flex items-center gap-1 ${
                      currentMonitorStatus?.current_status === 'operational'
                        ? 'text-green-700 dark:text-green-500'
                        : currentMonitorStatus?.current_status === 'degraded'
                          ? 'text-yellow-700 dark:text-yellow-500'
                          : currentMonitorStatus?.current_status ===
                              'major_outage'
                            ? 'text-red-700 dark:text-red-500'
                            : 'text-muted-foreground'
                    }`}
                  >
                    <span
                      className={`inline-block h-1.5 w-1.5 rounded-full ${
                        currentMonitorStatus?.current_status === 'operational'
                          ? 'bg-green-500'
                          : currentMonitorStatus?.current_status === 'degraded'
                            ? 'bg-yellow-500'
                            : currentMonitorStatus?.current_status ===
                                'major_outage'
                              ? 'bg-red-500'
                              : 'bg-gray-400'
                      }`}
                    ></span>
                    {currentMonitorStatus?.current_status === 'operational'
                      ? 'Healthy'
                      : currentMonitorStatus?.current_status === 'degraded'
                        ? 'Degraded'
                        : currentMonitorStatus?.current_status ===
                            'major_outage'
                          ? 'Major Outage'
                          : 'Unknown'}
                  </div>
                </div>
                {/* Mini Status Timeline */}
                <div className="flex gap-0.5 h-6 w-48">
                  {statusData?.buckets && statusData.buckets.length > 0
                    ? statusData.buckets
                        .slice(-48)
                        .map((bucket, idx) => (
                          <div
                            key={idx}
                            className={`flex-1 rounded-sm ${
                              bucket.status === 'operational'
                                ? 'bg-green-500'
                                : bucket.status === 'major_outage'
                                  ? 'bg-red-500'
                                  : bucket.status === 'degraded'
                                    ? 'bg-yellow-500'
                                    : 'bg-gray-300'
                            }`}
                            title={`${new Date(bucket.bucket_start).toLocaleString()}\nStatus: ${bucket.status}\nAvg: ${bucket.avg_response_time_ms?.toFixed(0) ?? 'N/A'}ms`}
                          />
                        ))
                    : Array.from({ length: 48 }).map((_, idx) => (
                        <div
                          key={idx}
                          className="flex-1 rounded-sm bg-gray-200 dark:bg-gray-700"
                        />
                      ))}
                </div>
              </>
            )}
          </div>

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                aria-label="Open menu"
                variant="ghost"
                size="icon"
                className="h-8 w-8"
              >
                <MoreVertical className="h-4 w-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem asChild>
                <Link to={`/projects/${projectSlug}/monitors/${monitor.id}`}>
                  <ExternalLink className="mr-2 h-4 w-4" />
                  View Details
                </Link>
              </DropdownMenuItem>
              <DropdownMenuItem
                className="text-destructive focus:text-destructive"
                onClick={onDelete}
              >
                <Trash2 className="mr-2 h-4 w-4" />
                Delete
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </CardContent>
    </Card>
  )
}

export function ProjectMonitors({ project }: ProjectMonitorsProps) {
  const queryClient = useQueryClient()
  const [runtimeEnvironmentId, setRuntimeEnvironmentId] = useState<
    number | null
  >(null)
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [monitorToDelete, setMonitorToDelete] =
    useState<MonitorResponse | null>(null)

  // Form with validation
  const form = useForm<CreateMonitorFormData>({
    resolver: zodResolver(createMonitorSchema),
    defaultValues: {
      name: '',
      monitor_type: 'http',
      check_interval_seconds: 60,
    },
  })

  const {
    data: monitors,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...listMonitorsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  // Fetch environments for the project
  const { data: environments, isLoading: isLoadingEnvironments } = useQuery({
    ...getEnvironmentsOptions({
      path: {
        project_id: project.id,
      },
    }),
  })

  const runtimeEnabled = project.preset !== 'docker-compose'
  const runtimeHealth = useQuery({
    ...getProjectRuntimeHealthOptions({ path: { project_id: project.id } }),
    enabled: runtimeEnabled,
    staleTime: 15_000,
    refetchInterval: 30_000,
  })
  const workerEnvironments = new Set(
    runtimeHealth.data
      ?.filter((workload) => workload.portless === true)
      .map((workload) => workload.environment_id) ?? []
  )
  const runtimeEnvironment =
    environments?.find(
      (environment) => environment.id === runtimeEnvironmentId
    ) ??
    environments?.find((environment) => environment.name === 'production') ??
    environments?.[0]
  const externalMonitors = visibleExternalMonitors(
    monitors ?? [],
    workerEnvironments
  )

  const createMutation = useMutation({
    ...createMonitorMutation(),
    meta: {
      errorTitle: 'Failed to create monitor',
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['listMonitors'] })
      await refetch()
      setIsCreateDialogOpen(false)
      form.reset()
      toast.success('Monitor created successfully!')
    },
  })

  const deleteMutation = useMutation({
    ...deleteMonitorMutation(),
    meta: {
      errorTitle: 'Failed to delete monitor',
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['listMonitors'] })
      await refetch()
      setMonitorToDelete(null)
      toast.success('Monitor deleted successfully!')
    },
  })

  const handleCreateMonitor = (data: CreateMonitorFormData) => {
    createMutation.mutate({
      path: {
        project_id: project.id,
      },
      body: data,
    })
  }

  const handleDeleteMonitor = (monitorId: number) => {
    deleteMutation.mutate({
      path: {
        monitor_id: monitorId,
      },
    })
  }

  if (error) {
    return (
      <div className="p-6">
        <ErrorAlert
          title="Failed to load monitors"
          description={
            error instanceof Error
              ? error.message
              : 'An unexpected error occurred'
          }
          retry={() => refetch()}
        />
      </div>
    )
  }

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h2 className="text-2xl font-bold tracking-tight">
            {runtimeEnabled ? 'Uptime and health' : 'Monitors'}
          </h2>
          <p className="text-muted-foreground">
            Monitor your service&apos;s uptime and performance
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="outline" asChild>
            <Link to={`/projects/${project.slug}/status-pages`}>
              <Globe className="size-4 sm:mr-2" />
              <span className="max-sm:sr-only">Status pages</span>
            </Link>
          </Button>
          {externalMonitors.length > 0 && (
            <CreateActionButton
              onClick={() => setIsCreateDialogOpen(true)}
              label="Create Monitor"
            />
          )}
        </div>
        <Dialog open={isCreateDialogOpen} onOpenChange={setIsCreateDialogOpen}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Create Monitor</DialogTitle>
              <DialogDescription>
                Set up a new monitor to track your service&apos;s uptime and
                performance.
              </DialogDescription>
            </DialogHeader>
            <Form {...form}>
              <form
                onSubmit={form.handleSubmit(handleCreateMonitor)}
                className="space-y-4"
              >
                <FormField
                  control={form.control}
                  name="name"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Monitor Name</FormLabel>
                      <FormControl>
                        <Input placeholder="Production API" {...field} />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="monitor_type"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Monitor Type</FormLabel>
                      <Select
                        onValueChange={field.onChange}
                        defaultValue={field.value}
                      >
                        <FormControl>
                          <SelectTrigger>
                            <SelectValue />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          <SelectItem value="http">HTTP</SelectItem>
                          <SelectItem value="https">HTTPS</SelectItem>
                          <SelectItem value="tcp">TCP</SelectItem>
                          <SelectItem value="ping">Ping</SelectItem>
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="environment_id"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Environment</FormLabel>
                      <Select
                        onValueChange={(value) =>
                          field.onChange(parseInt(value))
                        }
                        value={field.value?.toString()}
                      >
                        <FormControl>
                          <SelectTrigger>
                            <SelectValue
                              placeholder={
                                isLoadingEnvironments
                                  ? 'Loading...'
                                  : 'Select environment'
                              }
                            />
                          </SelectTrigger>
                        </FormControl>
                        <SelectContent>
                          {environments?.map((env) => (
                            <SelectItem key={env.id} value={env.id.toString()}>
                              {env.name}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="check_interval_seconds"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel>Check Interval (seconds)</FormLabel>
                      <FormControl>
                        <NumberInput
                          min={30}
                          name={field.name}
                          ref={field.ref}
                          onBlur={field.onBlur}
                          value={field.value}
                          onValueChange={(value) =>
                            field.onChange(value ?? Number.NaN)
                          }
                        />
                      </FormControl>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <DialogFooter>
                  <Button
                    variant="outline"
                    type="button"
                    onClick={() => setIsCreateDialogOpen(false)}
                  >
                    Cancel
                  </Button>
                  <Button type="submit" disabled={createMutation.isPending}>
                    {createMutation.isPending
                      ? 'Creating...'
                      : 'Create Monitor'}
                  </Button>
                </DialogFooter>
              </form>
            </Form>
          </DialogContent>
        </Dialog>
      </div>

      {/* Monitors List */}
      {runtimeEnabled && runtimeEnvironment && (
        <div className="space-y-4">
          <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
            <label
              htmlFor="runtime-health-environment"
              className="text-sm font-medium"
            >
              Runtime environment
            </label>
            <Select
              value={runtimeEnvironment.id.toString()}
              onValueChange={(value) => setRuntimeEnvironmentId(Number(value))}
            >
              <SelectTrigger
                id="runtime-health-environment"
                className="w-full sm:w-56"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {environments?.map((environment) => (
                  <SelectItem
                    key={environment.id}
                    value={environment.id.toString()}
                  >
                    {environment.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <RuntimeHealthPanel
            project={project}
            environment={runtimeEnvironment}
          />
        </div>
      )}

      {isLoading ? (
        <div className="space-y-3">
          {Array.from({ length: 3 }).map((_, i) => (
            <Card key={i}>
              <CardContent className="flex items-center justify-between p-4">
                <div className="flex items-center gap-4 flex-1">
                  <Skeleton className="h-10 w-10 rounded-md" />
                  <div className="space-y-2">
                    <Skeleton className="h-5 w-48" />
                    <Skeleton className="h-4 w-64" />
                  </div>
                </div>
                <div className="flex items-center gap-6">
                  <Skeleton className="h-4 w-20" />
                  <Skeleton className="h-8 w-8" />
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      ) : externalMonitors.length > 0 ? (
        <div className="space-y-3">
          {externalMonitors.map((monitor) => (
            <MonitorCard
              key={monitor.id}
              monitor={monitor}
              projectSlug={project.slug}
              onDelete={() => setMonitorToDelete(monitor)}
              environment={environments?.find(
                (env) => env.id === monitor.environment_id
              )}
            />
          ))}
        </div>
      ) : (
        <EmptyState
          size="compact"
          icon={Activity}
          title={runtimeEnabled ? 'No external monitors' : 'No monitors yet'}
          description={
            runtimeEnabled
              ? 'Runtime health is shown above. Add an independent HTTP or TCP monitor when the service exposes an endpoint.'
              : "Create a monitor to track this service's uptime and response time."
          }
          action={
            <Button onClick={() => setIsCreateDialogOpen(true)}>
              <Plus className="mr-2 size-4" />
              Create monitor
            </Button>
          }
        />
      )}

      <ConfirmDeleteDialog
        open={monitorToDelete !== null}
        onOpenChange={(open) => {
          if (!open) setMonitorToDelete(null)
        }}
        resourceType="monitor"
        resourceName={monitorToDelete?.name ?? ''}
        consequences="Its uptime and response-time history is deleted with it and checks stop running."
        isPending={deleteMutation.isPending}
        onConfirm={() => {
          if (monitorToDelete) handleDeleteMonitor(monitorToDelete.id)
        }}
      />
    </div>
  )
}
