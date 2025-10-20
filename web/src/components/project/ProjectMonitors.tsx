import {
  listMonitorsOptions,
  createMonitorMutation,
  deleteMonitorMutation,
  getBucketedStatusOptions,
  getEnvironmentsOptions,
  getCurrentMonitorStatusOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ProjectResponse, MonitorResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { ErrorAlert } from '@/components/utils/ErrorAlert'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Activity,
  Plus,
  Trash2,
  ExternalLink,
  MoreVertical,
} from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
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
  DialogTrigger,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
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
    .number()
    .min(30, 'Check interval must be at least 30 seconds'),
})

type CreateMonitorFormData = z.infer<typeof createMonitorSchema>

interface MonitorCardProps {
  monitor: MonitorResponse
  projectSlug: string
  onDelete: () => void
}

function MonitorCard({ monitor, projectSlug, onDelete }: MonitorCardProps) {
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

  const {
    data: currentMonitorStatus,
    isLoading: isLoadingCurrentMonitorStatus,
    error: currentMonitorStatusError,
    refetch: refetchCurrentMonitorStatus,
  } = useQuery({
    ...getCurrentMonitorStatusOptions({
      path: {
        monitor_id: monitor.id,
      },
      query: {
        start_time: startDate ? startDate.toISOString() : undefined,
        end_time: endDate ? endDate.toISOString() : undefined,
      },
    }),
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
            <div className="flex items-center gap-2">
              <div className="text-lg font-semibold">
                {uptimePercentage?.toFixed(0) ?? 'N/A'}%
              </div>
              <div className="text-xs text-green-500 flex items-center gap-1">
                <span className="inline-block h-1.5 w-1.5 rounded-full bg-green-500"></span>
                Healthy
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
          </div>

          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" className="h-8 w-8">
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
  const [isCreateDialogOpen, setIsCreateDialogOpen] = useState(false)
  const [monitorToDelete, setMonitorToDelete] = useState<number | null>(null)

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
          <h2 className="text-2xl font-bold tracking-tight">Monitors</h2>
          <p className="text-muted-foreground">
            Monitor your project's uptime and performance
          </p>
        </div>
        <Dialog open={isCreateDialogOpen} onOpenChange={setIsCreateDialogOpen}>
          <DialogTrigger asChild>
            <Button>
              <Plus className="mr-2 h-4 w-4" />
              Create Monitor
            </Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Create Monitor</DialogTitle>
              <DialogDescription>
                Set up a new monitor to track your project's uptime and
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
                        <Input
                          type="number"
                          min="30"
                          {...field}
                          onChange={(e) =>
                            field.onChange(parseInt(e.target.value) || 60)
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
      ) : monitors && monitors.length > 0 ? (
        <div className="space-y-3">
          {monitors.map((monitor) => (
            <MonitorCard
              key={monitor.id}
              monitor={monitor}
              projectSlug={project.slug}
              onDelete={() => setMonitorToDelete(monitor.id)}
            />
          ))}
        </div>
      ) : (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <Activity className="h-12 w-12 text-muted-foreground mb-4" />
            <h3 className="text-lg font-semibold mb-2">No monitors yet</h3>
            <p className="text-sm text-muted-foreground mb-4">
              Create your first monitor to start tracking uptime and
              performance.
            </p>
            <Button onClick={() => setIsCreateDialogOpen(true)}>
              <Plus className="mr-2 h-4 w-4" />
              Create Monitor
            </Button>
          </CardContent>
        </Card>
      )}

      {/* Delete Confirmation Dialog */}
      <AlertDialog
        open={monitorToDelete !== null}
        onOpenChange={() => setMonitorToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete Monitor</AlertDialogTitle>
            <AlertDialogDescription>
              Are you sure you want to delete this monitor? This action cannot
              be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() =>
                monitorToDelete && handleDeleteMonitor(monitorToDelete)
              }
              className="bg-destructive hover:bg-destructive/90"
            >
              {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}
