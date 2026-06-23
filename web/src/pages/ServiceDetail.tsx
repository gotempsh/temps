import {
  deleteServiceMutation,
  getProjectsOptions,
  getServiceOptions,
  getServicePreviewEnvironmentVariablesMaskedOptions,
  linkServiceToProjectMutation,
  listServiceProjectsOptions,
  startServiceMutation,
  stopServiceMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { cn } from '@/lib/utils'
import { listExternalServiceBackupsOptions } from '@/lib/external-service-backups'
import { ClusterHealthPanel } from '@/components/storage/ClusterHealthPanel'
import { MonitoringCard } from '@/components/storage/MonitoringCard'
import { EditServiceDialog } from '@/components/storage/EditServiceDialog'
import { ServiceResourcesPanel } from '@/components/storage/ServiceResourcesPanel'
import { MajorUpgradeDialog } from '@/components/storage/MajorUpgradeDialog'
import {
  ServiceHealthBadge,
  ServiceHealthCard,
} from '@/components/storage/ServiceHealthCard'
import { WalHealthPanel } from '@/components/storage/WalHealthPanel'
import { TriggerBackupDialog } from '@/components/storage/TriggerBackupDialog'
import { UpgradeServiceDialog } from '@/components/storage/UpgradeServiceDialog'
import { listPgUpgrades, phaseIndex, PG_UPGRADE_PHASES, isTerminal } from '@/lib/pg-upgrades'
import { Alert, AlertDescription } from '@/components/ui/alert'
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
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { CopyButton } from '@/components/ui/copy-button'
import { EnvVariablesDisplay } from '@/components/ui/env-variables-display'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { ServiceLogo } from '@/components/ui/service-logo'
import { TimeAgo } from '@/components/utils/TimeAgo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { maskValue, shouldMaskValue } from '@/lib/masking'
import { formatBytes } from '@/lib/utils'
import { iconForServiceType } from '@/lib/serviceIcons'
import {
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  AlertCircle,
  Activity,
  ArrowLeft,
  ArrowUpCircle,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Clock,
  Database,
  Eye,
  EyeOff,
  HardDrive,
  Loader2,
  MoreVertical,
  Pencil,
  Plus,
  Radio,
  RefreshCcw,
  RotateCcw,
  Server,
  Trash2,
  XCircle,
} from 'lucide-react'
import { format } from 'date-fns'
import { useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { toast } from 'sonner'

/**
 * Pick the role label to render for a cluster member.
 *
 * `live_state` (from the pg_auto_failover monitor) is the source of
 * truth for "primary" — it flips the moment failover or a manual
 * promotion lands. `role` (from `service_members.role`) is config
 * state: `monitor` for the orchestrator, `replica` for every data
 * node. Showing the stored `role` for primaries was the bug behind
 * "two primaries" displayed after a failover.
 *
 * Rules:
 *   - monitor row → "monitor" (no live_state ever)
 *   - live_state in {primary, single} → "primary"
 *   - live_state set to anything else (secondary, catchingup, …) → "replica"
 *   - live_state missing (monitor unreachable, just-created) → fall back
 *     to stored `role` so the UI doesn't suddenly say "replica" for
 *     every node when the monitor blips
 */
/**
 * Compact wall-clock duration: `<1s`, `42s`, `1m 5s`, `2h 13m`.
 * Sub-second durations render as `<1s` so a backup that legitimately
 * finished doesn't look like a zero-duration ghost row.
 */
function formatShortDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '<1s'
  const seconds = Math.floor(ms / 1000)
  if (seconds === 0) return '<1s'
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

function memberDisplayRole(member: {
  role: string
  live_state?: string | null
}): string {
  if (member.role === 'monitor') return 'monitor'
  const live = member.live_state
  if (live === 'primary' || live === 'single') return 'primary'
  if (live) return 'replica'
  return member.role
}

export function ServiceDetail() {
  const { id } = useParams<{ id: string }>()
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [isDeleteDialogOpen, setIsDeleteDialogOpen] = useState(false)
  const [deleteConfirmName, setDeleteConfirmName] = useState('')
  const [isUpgradeDialogOpen, setIsUpgradeDialogOpen] = useState(false)
  const [isMajorUpgradeDialogOpen, setIsMajorUpgradeDialogOpen] = useState(false)
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [isBackupDialogOpen, setIsBackupDialogOpen] = useState(false)
  const [isStopDialogOpen, setIsStopDialogOpen] = useState(false)
  const [memberToRemove, setMemberToRemove] = useState<{
    id: number
    container_name: string
    role: string
  } | null>(null)
  const [memberToPromote, setMemberToPromote] = useState<{
    id: number
    container_name: string
  } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [prevStatus, setPrevStatus] = useState<string | undefined>(undefined)
  const [visibleParameters, setVisibleParameters] = useState<Set<string>>(
    new Set()
  )

  const {
    data: service,
    isLoading,
    error: queryError,
    refetch,
  } = useQuery({
    ...getServiceOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
    refetchInterval: (query) => {
      const status = query.state.data?.service?.status
      return status === 'creating' ? 2000 : false
    },
  })

  // Query for PostgreSQL major-version upgrades. Only relevant for postgres
  // services; harmless for others (the query is enabled conditionally below).
  const isPostgres = service?.service?.service_type === 'postgres'
  const { data: pgUpgrades } = useQuery({
    queryKey: ['pg-upgrades', id],
    queryFn: () => listPgUpgrades(parseInt(id!)),
    enabled: !!id && isPostgres,
    // Poll every 3s while any upgrade is still running so the card reflects
    // phase progression without a manual refresh.
    refetchInterval: (query) => {
      const rows = query.state.data
      if (!rows) return false
      return rows.some((u) => !isTerminal(u.status)) ? 3000 : false
    },
  })


  // Query for environment variables
  const {
    data: envVars,
    isLoading: envVarsLoading,
    error: envVarsError,
  } = useQuery({
    ...getServicePreviewEnvironmentVariablesMaskedOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
    staleTime: 5 * 60 * 1000, // Cache for 5 minutes
  })

  // Query for linked projects
  const {
    data: linkedProjectsResponse,
    isLoading: linkedProjectsLoading,
    refetch: refetchLinkedProjects,
  } = useQuery({
    ...listServiceProjectsOptions({
      path: { id: parseInt(id!) },
    }),
    enabled: !!id,
  })

  // All projects for the link popover
  const { data: allProjectsData } = useQuery({
    ...getProjectsOptions({ query: { page: 1, per_page: 100 } }),
  })

  // Backups for this service — single DB-only query, no S3 fan-out.
  // Replaces the previous multi-source fan-out that issued one slow S3 scan
  // per configured source on every page load.
  const serviceId = id ? parseInt(id) : undefined
  const [backupsPage, setBackupsPage] = useState(1)
  const BACKUPS_PAGE_SIZE = 5

  const {
    data: serviceBackupsData,
    isLoading: isLoadingBackups,
    isFetching: isFetchingBackups,
    refetch: refetchBackups,
  } = useQuery({
    ...listExternalServiceBackupsOptions(serviceId, backupsPage, BACKUPS_PAGE_SIZE),
    enabled: !!serviceId,
  })

  const serviceBackups = serviceBackupsData?.backups ?? []
  const backupsTotalPages = Math.max(
    1,
    Math.ceil((serviceBackupsData?.total ?? 0) / BACKUPS_PAGE_SIZE),
  )

  useEffect(() => {
    if (backupsPage > backupsTotalPages) {
      setBackupsPage(backupsTotalPages)
    }
  }, [backupsPage, backupsTotalPages])

  const paginatedBackups = serviceBackups

  const backupsPageWindow = useMemo(() => {
    const windowSize = Math.min(5, backupsTotalPages)
    const start = Math.max(
      1,
      Math.min(
        backupsPage - Math.floor(windowSize / 2),
        backupsTotalPages - windowSize + 1,
      ),
    )
    return Array.from({ length: windowSize }, (_, idx) => start + idx)
  }, [backupsPage, backupsTotalPages])

  const [isLinkPopoverOpen, setIsLinkPopoverOpen] = useState(false)

  const linkService = useMutation({
    ...linkServiceToProjectMutation(),
    meta: { errorTitle: 'Failed to link project' },
    onSuccess: () => {
      toast.success('Project linked successfully')
      refetchLinkedProjects()
      setIsLinkPopoverOpen(false)
    },
  })

  useEffect(() => {
    if (service) {
      setBreadcrumbs([
        { label: 'Databases', href: '/storage' },
        {
          label: service.service.name || 'Service Details',
          href: `/storage/${id}`,
        },
      ])
    } else {
      setBreadcrumbs([
        { label: 'Databases', href: '/storage' },
        { label: 'Service Details', href: `/storage/${id}` },
      ])
    }
  }, [setBreadcrumbs, id, service])

  usePageTitle(service?.service?.name || 'Service Details')

  // Notify when cluster creation completes or fails
  useEffect(() => {
    const currentStatus = service?.service?.status
    if (prevStatus === 'creating' && currentStatus === 'running') {
      toast.success('Cluster created successfully')
    } else if (prevStatus === 'creating' && currentStatus === 'failed') {
      toast.error('Cluster creation failed')
    }
    if (currentStatus) {
      setPrevStatus(currentStatus)
    }
  }, [service?.service?.status, prevStatus])

  const startService = useMutation({
    ...startServiceMutation(),
    meta: {
      errorTitle: 'Failed to start service',
    },
    onSuccess: () => {
      refetch()
      setError(null)
    },
  })

  const stopService = useMutation({
    ...stopServiceMutation(),
    meta: {
      errorTitle: 'Failed to stop service',
    },
    onSuccess: () => {
      toast.success('Service stopped successfully')
      refetch()
    },
  })

  const retryCluster = useMutation({
    mutationFn: async (options: {
      path: { id: number }
      body: { members: { role: string; node_id?: number }[] }
    }) => {
      const response = await fetch(
        `/api/external-services/${options.path.id}/retry`,
        {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          credentials: 'include',
          body: JSON.stringify(options.body),
        }
      )
      if (!response.ok) {
        const error = await response.json().catch(() => ({}))
        throw new Error(error.detail || 'Retry failed')
      }
      return response.json()
    },
    onSuccess: () => {
      toast.success('Cluster retry initiated')
      refetch()
    },
    onError: (error: Error) => {
      toast.error('Failed to retry cluster', {
        description: error.message,
      })
    },
  })

  // Remove a single member from a running cluster. The backend refuses
  // monitor / current primary / quorum-violating removals — surface the
  // detail message verbatim when that happens.
  const removeMember = useMutation({
    mutationFn: async (options: {
      serviceId: number
      memberId: number
    }) => {
      const response = await fetch(
        `/api/external-services/${options.serviceId}/members/${options.memberId}`,
        {
          method: 'DELETE',
          credentials: 'include',
        }
      )
      if (!response.ok) {
        const err = await response.json().catch(() => ({}))
        throw new Error(err.detail || 'Failed to remove member')
      }
    },
    onSuccess: () => {
      toast.success('Cluster member removed')
      setMemberToRemove(null)
      refetch()
      queryClient.invalidateQueries({
        queryKey: ['cluster-health', parseInt(id!)],
      })
    },
    onError: (error: Error) => {
      toast.error('Failed to remove cluster member', {
        description: error.message,
      })
    },
  })

  // Promote a replica to primary by triggering a pg_auto_failover
  // failover. The backend runs `pg_autoctl perform promotion` inside
  // the chosen container; the monitor demotes the current primary and
  // the role reconciler refreshes role-aliased VIPs on its next tick.
  const promoteMember = useMutation({
    mutationFn: async (options: {
      serviceId: number
      memberId: number
    }) => {
      const response = await fetch(
        `/api/external-services/${options.serviceId}/members/${options.memberId}/promote`,
        {
          method: 'POST',
          credentials: 'include',
        }
      )
      if (!response.ok) {
        const err = await response.json().catch(() => ({}))
        throw new Error(err.detail || 'Failed to promote member')
      }
    },
    onSuccess: () => {
      toast.success('Promotion initiated', {
        description:
          'pg_auto_failover is demoting the current primary; the table will update as roles flip.',
      })
      setMemberToPromote(null)

      // Refetch immediately so the dialog closes onto fresh data,
      // then poll a few more times because pg_auto_failover's FSM
      // takes a beat to transition (wait_primary → primary) and the
      // reconciler runs every 5s. After ~10s either the table reflects
      // reality or the promotion failed silently and the existing 5s
      // poll will catch the next steady state.
      const ping = () => {
        refetch()
        queryClient.invalidateQueries({
          queryKey: ['cluster-health', parseInt(id!)],
        })
      }
      ping()
      const delays = [1500, 3500, 6500, 10000]
      delays.forEach((ms) => setTimeout(ping, ms))
    },
    onError: (error: Error) => {
      toast.error('Failed to promote cluster member', {
        description: error.message,
      })
    },
  })

  const deleteService = useMutation({
    ...deleteServiceMutation(),
    meta: {
      errorTitle: 'Failed to delete service',
    },
    onSuccess: () => {
      toast.success('Service deleted successfully')
      navigate('/storage')
    },
    onError: (error: any) => {
      toast.error('Failed to delete service', {
        description:
          error.detail || error.message || 'An unexpected error occurred',
      })
      setIsDeleteDialogOpen(false)
    },
  })

  const handleServiceAction = async () => {
    if (!service) return

    if (service.service.status === 'running') {
      setIsStopDialogOpen(true)
    } else if (service.service.status === 'stopped') {
      // Start can take 5–15s for Postgres (reconcile + recreate path).
      // toast.promise surfaces all three states without blocking the UI,
      // matching the rollback/promote patterns elsewhere in the app.
      toast.promise(
        startService.mutateAsync({ path: { id: parseInt(id!) } }),
        {
          loading: `Starting ${service.service.name}…`,
          success: `${service.service.name} started`,
          error: (err: Error) =>
            err?.message
              ? `Failed to start ${service.service.name}: ${err.message}`
              : `Failed to start ${service.service.name}`,
        }
      )
    }
  }

  const handleStop = async () => {
    stopService.mutate(
      { path: { id: parseInt(id!) } },
      { onSettled: () => setIsStopDialogOpen(false) }
    )
  }

  const handleDelete = async () => {
    if (deleteConfirmName.trim() !== service?.service?.name) return
    deleteService.mutate({ path: { id: parseInt(id!) } })
  }

  if (isLoading) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-4 space-y-6 md:p-6">
          <div className="h-8 w-32 bg-muted rounded animate-pulse" />
          <Card>
            <CardHeader>
              <div className="space-y-2">
                <div className="h-5 w-40 bg-muted rounded animate-pulse" />
                <div className="h-4 w-24 bg-muted rounded animate-pulse" />
              </div>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="h-4 w-full bg-muted rounded animate-pulse" />
                <div className="h-4 w-3/4 bg-muted rounded animate-pulse" />
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    )
  }

  if (queryError || !service) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="p-4 space-y-6 md:p-6">
          <div className="flex flex-col items-center justify-center py-12 text-center">
            <p className="text-sm text-muted-foreground mb-4">
              Failed to load service details
            </p>
            <Button
              variant="outline"
              onClick={() => refetch()}
              className="gap-2"
            >
              <RefreshCcw className="h-4 w-4" />
              Try again
            </Button>
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="p-4 space-y-6 md:p-6">
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-3">
            <Link to="/storage">
              <Button variant="ghost" size="icon">
                <ArrowLeft className="h-4 w-4" />
              </Button>
            </Link>
            <ServiceLogo
              service={service.service.service_type}
              className="h-8 w-8"
            />
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2 flex-wrap">
                <h1 className="text-xl font-semibold sm:text-2xl">
                  {service.service.name}
                </h1>
                <Badge
                  variant={
                    service.service.status === 'running'
                      ? 'default'
                      : service.service.status === 'stopped'
                        ? 'secondary'
                        : 'outline'
                  }
                  className="capitalize"
                >
                  {service.service.status}
                </Badge>
                {service.service.status === 'running' ? (
                  <ServiceHealthBadge serviceId={parseInt(id!)} />
                ) : null}
                <Badge variant="outline" className="gap-1.5">
                  <ServiceLogo
                    service={service.service.service_type}
                    className="h-3 w-3"
                  />
                  {service.service.service_type}
                </Badge>
                {service.service.topology === 'cluster' && (
                  <Badge variant="outline" className="gap-1.5">
                    <Server className="h-3 w-3" />
                    Cluster
                  </Badge>
                )}
              </div>
              <p className="text-sm text-muted-foreground">
                Created <TimeAgo date={service.service.created_at} />
              </p>
            </div>
          </div>

          <div className="flex items-center gap-2 self-start sm:self-auto flex-wrap">
            {/*
              Linked projects: collapsed into a header chip so the body of the
              page can stay focused on Health + Configuration. Click to see the
              list and link more projects inline.
            */}
            <Popover
              open={isLinkPopoverOpen}
              onOpenChange={setIsLinkPopoverOpen}
            >
              <PopoverTrigger asChild>
                <Button variant="outline" size="sm" className="gap-2">
                  <Plus className="h-4 w-4" />
                  {linkedProjectsLoading ? (
                    <Loader2 className="h-3 w-3 animate-spin" />
                  ) : (
                    <>
                      <span className="hidden sm:inline">{linkedProjectsResponse?.length || 0} linked</span>
                      <span className="sm:hidden">{linkedProjectsResponse?.length || 0}</span>
                    </>
                  )}
                </Button>
              </PopoverTrigger>
              <PopoverContent className="w-[320px] p-0" align="end">
                <div className="border-b p-3">
                  <p className="text-xs font-medium text-muted-foreground">
                    Linked projects
                  </p>
                  <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
                    Linking creates a dedicated{' '}
                    <code className="rounded bg-muted px-1 py-0.5 font-mono">
                      {'<project>_<env>'}
                    </code>{' '}
                    database per environment. No extra services are spun up.
                  </p>
                  {linkedProjectsLoading ? (
                    <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
                      <Loader2 className="h-3 w-3 animate-spin" />
                      Loading…
                    </div>
                  ) : linkedProjectsResponse &&
                    linkedProjectsResponse.length > 0 ? (
                    <ul className="mt-2 space-y-1">
                      {linkedProjectsResponse.map((link) => (
                        <li
                          key={link.id}
                          className="flex items-center justify-between gap-2 text-sm"
                        >
                          <span className="truncate">
                            {link.project.slug}
                          </span>
                          <Link
                            to={`/projects/${link.project.slug}`}
                            className="text-xs text-muted-foreground hover:text-foreground"
                          >
                            View →
                          </Link>
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p className="mt-2 text-xs text-muted-foreground">
                      No projects yet.
                    </p>
                  )}
                </div>
                <Command>
                  <CommandInput placeholder="Link a project..." />
                  <CommandList>
                    <CommandEmpty>No projects found.</CommandEmpty>
                    <CommandGroup>
                      {allProjectsData?.projects
                        ?.filter(
                          (p) =>
                            !linkedProjectsResponse?.some(
                              (lp) => lp.project.id === p.id,
                            ),
                        )
                        .map((project) => (
                          <CommandItem
                            key={project.id}
                            value={project.slug}
                            onSelect={() => {
                              linkService.mutate({
                                path: { id: parseInt(id!) },
                                body: { project_id: project.id },
                              })
                            }}
                          >
                            {project.slug}
                          </CommandItem>
                        ))}
                    </CommandGroup>
                  </CommandList>
                </Command>
              </PopoverContent>
            </Popover>

            {service.service.status === 'running' && (
              <Link to={`/storage/${id}/monitoring`}>
                <Button variant="outline" size="sm" className="gap-2">
                  <Activity className="h-4 w-4" />
                  <span className="hidden sm:inline">Monitoring</span>
                </Button>
              </Link>
            )}
            <Link to={`/storage/${id}/browse`}>
              <Button variant="outline" size="sm" className="gap-2">
                <Database className="h-4 w-4" />
                <span className="hidden sm:inline">Browse Data</span>
              </Button>
            </Link>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" className="h-8 w-8">
                  <MoreVertical className="h-4 w-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => setIsBackupDialogOpen(true)}>
                  <HardDrive className="h-4 w-4 mr-2" />
                  Backup
                </DropdownMenuItem>
                <DropdownMenuItem
                  onClick={() => navigate(`/storage/${parseInt(id!)}/restore`)}
                >
                  <RotateCcw className="h-4 w-4 mr-2" />
                  Restore…
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => setIsEditDialogOpen(true)}>
                  <Pencil className="h-4 w-4 mr-2" />
                  Edit
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => setIsUpgradeDialogOpen(true)}>
                  <ArrowUpCircle className="h-4 w-4 mr-2" />
                  Upgrade
                </DropdownMenuItem>
                {service.service.service_type === 'postgres' ? (
                  <DropdownMenuItem
                    onClick={() => setIsMajorUpgradeDialogOpen(true)}
                  >
                    <ArrowUpCircle className="h-4 w-4 mr-2" />
                    Major Version Upgrade…
                  </DropdownMenuItem>
                ) : null}
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  onClick={handleServiceAction}
                  disabled={
                    service.service.status === 'creating' ||
                    startService.isPending ||
                    stopService.isPending
                  }
                  className={
                    service.service.status === 'running'
                      ? 'text-destructive focus:text-destructive'
                      : ''
                  }
                >
                  {(startService.isPending || stopService.isPending) ? (
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  ) : service.service.status === 'running' ? (
                    <AlertCircle className="h-4 w-4 mr-2" />
                  ) : (
                    <RefreshCcw className="h-4 w-4 mr-2" />
                  )}
                  {service.service.status === 'running'
                    ? 'Stop'
                    : service.service.status === 'creating'
                      ? 'Creating...'
                      : 'Start'}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  onClick={() => setIsDeleteDialogOpen(true)}
                  className="text-destructive focus:text-destructive"
                >
                  <Trash2 className="h-4 w-4 mr-2" />
                  Delete
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </div>

        {error && (
          <Alert variant="destructive">
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}

        <div className="grid gap-6">
          {/*
            Health is the highest-signal block on this page, so it sits
            directly under the header. Linked Projects moved into the header
            chip, so Configuration surfaces much sooner now.
          */}
          {service.service.status === 'running' ? (
            <ServiceHealthCard serviceId={parseInt(id!)} />
          ) : null}

          {/*
            Postgres-only WAL bloat / archive misconfiguration surface. Renders
            nothing when there are no warnings, so it's safe to mount
            unconditionally for non-Postgres services (the component itself
            checks the type and bails before fetching).
          */}
          {service.service.status === 'running' ? (
            <WalHealthPanel
              serviceId={parseInt(id!)}
              serviceType={service.service.service_type}
            />
          ) : null}

          {service.service.status === 'running' && (
            <MonitoringCard
              serviceId={service.service.id}
              engine={service.service.service_type}
              dockerImage={service.current_parameters?.docker_image ?? undefined}
              metricsEnabled={service.service.metrics_enabled ?? false}
              onMonitoringChange={() => refetch()}
            />
          )}

          {/* Cluster Creation Progress */}
          {service.service.topology === 'cluster' &&
            service.service.status === 'creating' && (
              <Alert>
                <Loader2 className="h-4 w-4 animate-spin" />
                <AlertDescription>
                  <span className="font-medium">
                    Creating cluster members...
                  </span>{' '}
                  This may take a minute. Members will appear below as they are
                  provisioned.
                </AlertDescription>
              </Alert>
            )}

          {/* Cluster Creation Failed */}
          {service.service.topology === 'cluster' &&
            service.service.status === 'failed' && (
              <Alert variant="destructive">
                <AlertCircle className="h-4 w-4" />
                <AlertDescription className="flex items-center justify-between gap-4">
                  <div>
                    <span className="font-medium">
                      Cluster creation failed.
                    </span>{' '}
                    {(service.service as Record<string, unknown>).error_message
                      ? String(
                          (service.service as Record<string, unknown>)
                            .error_message
                        )
                      : 'An unknown error occurred.'}
                  </div>
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={retryCluster.isPending}
                    onClick={() => {
                      // Reconstruct members from preserved service_members records,
                      // or send empty array to let the backend reconstruct.
                      const members =
                        service.service.members &&
                        service.service.members.length > 0
                          ? service.service.members.map(
                              (m: { role: string; node_id?: number | null }) => ({
                                role: m.role,
                                node_id: m.node_id ?? undefined,
                              })
                            )
                          : []
                      retryCluster.mutate({
                        path: { id: parseInt(id!) },
                        body: { members },
                      })
                    }}
                  >
                    {retryCluster.isPending ? (
                      <Loader2 className="h-4 w-4 animate-spin mr-1" />
                    ) : (
                      <RefreshCcw className="h-4 w-4 mr-1" />
                    )}
                    Retry
                  </Button>
                </AlertDescription>
              </Alert>
            )}

          {/* Cluster Members Section */}
          {service.service.topology === 'cluster' &&
            service.service.members &&
            service.service.members.length > 0 && (
              <Card>
                <CardHeader>
                  <div className="flex items-start justify-between gap-2">
                    <div>
                      <CardTitle className="flex items-center gap-2">
                        <span>Cluster Members</span>
                        <Badge variant="outline">
                          {service.service.members.length}
                        </Badge>
                      </CardTitle>
                      <CardDescription>
                        pg_auto_failover cluster nodes
                      </CardDescription>
                    </div>
                    {/* Scaling is only safe while the cluster is healthy and
                        a monitor exists. Hide the button entirely otherwise
                        rather than opening a dialog that will fail. */}
                    {service.service.status === 'running' &&
                      service.service.service_type === 'postgres' &&
                      service.service.members.some(
                        (m) => m.role === 'monitor'
                      ) && (
                        <Button variant="outline" size="sm" asChild>
                          <Link to={`/storage/${id}/members/add`}>
                            <Plus className="h-4 w-4 mr-1" />
                            Add Replica
                          </Link>
                        </Button>
                      )}
                  </div>
                </CardHeader>
                <CardContent>
                  <div className="space-y-2">
                    {service.service.members.map((member) => {
                      // The backend rejects removal of monitor + current
                      // primary + members below quorum; mirror those rules
                      // here so the UI doesn't show a button that will
                      // 400. Quorum check uses the wire-side member list:
                      // 2 data members minimum to keep HA.
                      const dataMembers = (
                        service.service.members ?? []
                      ).filter((m) => m.role !== 'monitor')
                      const wouldBreakQuorum =
                        member.role !== 'monitor' && dataMembers.length <= 2
                      // "Is this currently the primary?" is a runtime
                      // question — read live_state, not the stored role
                      // (which is now `replica` for every data node).
                      const isLivePrimary =
                        memberDisplayRole(member) === 'primary'
                      const removable =
                        member.role !== 'monitor' &&
                        !isLivePrimary &&
                        !wouldBreakQuorum &&
                        service.service.status === 'running'
                      // Promote: any running data member that isn't
                      // already the primary or the monitor. Backend
                      // re-validates so this is purely UI courtesy.
                      const promotable =
                        member.role !== 'monitor' &&
                        !isLivePrimary &&
                        member.status === 'running' &&
                        service.service.status === 'running'
                      return (
                        <div
                          key={member.id}
                          className="flex items-center justify-between gap-2 p-3 rounded-md border border-border"
                        >
                          <div className="flex items-center gap-3 min-w-0">
                            {member.status === 'creating' ? (
                              <Loader2 className="h-4 w-4 animate-spin text-muted-foreground flex-shrink-0" />
                            ) : (
                              <Server className="h-4 w-4 text-muted-foreground flex-shrink-0" />
                            )}
                            <div className="flex flex-col min-w-0">
                              <div className="flex items-center gap-2">
                                <span className="font-mono text-sm truncate">
                                  {member.container_name}
                                </span>
                                <Badge
                                  variant={
                                    memberDisplayRole(member) === 'primary'
                                      ? 'default'
                                      : 'secondary'
                                  }
                                  className="capitalize text-xs"
                                >
                                  {memberDisplayRole(member)}
                                </Badge>
                              </div>
                              <div className="flex items-center gap-2 text-xs text-muted-foreground">
                                {member.hostname && (
                                  <span>{member.hostname}</span>
                                )}
                                {member.port && <span>:{member.port}</span>}
                                {member.node_id && (
                                  <span className="ml-1">
                                    (node {member.node_id})
                                  </span>
                                )}
                              </div>
                            </div>
                          </div>
                          <div className="flex items-center gap-2 flex-shrink-0">
                            <Badge
                              variant={
                                member.status === 'running'
                                  ? 'default'
                                  : member.status === 'failed'
                                    ? 'destructive'
                                    : member.status === 'creating'
                                      ? 'outline'
                                      : 'secondary'
                              }
                              className="capitalize"
                            >
                              {member.status === 'creating' && (
                                <Loader2 className="h-3 w-3 animate-spin mr-1" />
                              )}
                              {member.status}
                            </Badge>
                            {promotable && (
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-8 w-8 text-muted-foreground hover:text-foreground"
                                aria-label={`Promote ${member.container_name} to primary`}
                                title="Promote to primary"
                                onClick={() =>
                                  setMemberToPromote({
                                    id: member.id,
                                    container_name: member.container_name,
                                  })
                                }
                              >
                                <ArrowUpCircle className="h-4 w-4" />
                              </Button>
                            )}
                            {removable && (
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-8 w-8 text-muted-foreground hover:text-destructive"
                                aria-label={`Remove ${member.container_name}`}
                                onClick={() =>
                                  setMemberToRemove({
                                    id: member.id,
                                    container_name: member.container_name,
                                    role: member.role,
                                  })
                                }
                              >
                                <Trash2 className="h-4 w-4" />
                              </Button>
                            )}
                          </div>
                        </div>
                      )
                    })}
                  </div>
                </CardContent>
              </Card>
            )}

          {/* Per-cluster live health panel — reads from pg_auto_failover monitor */}
          {service.service.topology === 'cluster' &&
            service.service.service_type === 'postgres' && (
              <ClusterHealthPanel serviceId={service.service.id} />
            )}

          {/* Resources: container runtime, live CPU/mem, applied limits */}
          <ServiceResourcesPanel
            serviceId={service.service.id}
            serviceName={service.service.name}
          />

          {/* Service Configuration Section */}
          <Card>
            <CardHeader>
              <CardTitle>Configuration</CardTitle>
              <CardDescription>Current service parameters</CardDescription>
            </CardHeader>
            <CardContent>
              {service.current_parameters &&
              Object.keys(service.current_parameters).length > 0 ? (
                <dl className="divide-y divide-border">
                  {Object.entries(service.current_parameters).map(
                    ([key, value]) => {
                      const isSensitive = shouldMaskValue(key)
                      const isVisible = visibleParameters.has(key)
                      // Coerce non-primitive values to a JSON string so a
                      // future structured sub-block (`resources`, etc.)
                      // doesn't crash the page with "Objects are not valid
                      // as a React child". Primitives render unchanged.
                      const safeValue =
                        value != null && typeof value === 'object'
                          ? JSON.stringify(value)
                          : value
                      const displayValue =
                        isSensitive && !isVisible
                          ? maskValue(safeValue)
                          : safeValue
                      const hasValue = Boolean(safeValue)

                      return (
                        <div
                          key={key}
                          className="grid grid-cols-1 gap-1 py-3 sm:grid-cols-3 sm:gap-4"
                        >
                          <dt className="text-sm font-medium capitalize text-foreground">
                            {key
                              .replace(/_/g, ' ')
                              .replace(/\b\w/g, (char) => char.toUpperCase())}
                          </dt>
                          <dd className="flex min-w-0 items-center gap-2 sm:col-span-2">
                            <span className="min-w-0 flex-1 break-all font-mono text-sm text-muted-foreground tabular-nums">
                              {hasValue ? (
                                displayValue
                              ) : (
                                <span className="italic">Not set</span>
                              )}
                            </span>
                            {hasValue && isSensitive && (
                              <Button
                                variant="ghost"
                                size="icon"
                                className="h-8 w-8 shrink-0"
                                onClick={() => {
                                  setVisibleParameters((prev) => {
                                    const next = new Set(prev)
                                    if (next.has(key)) {
                                      next.delete(key)
                                    } else {
                                      next.add(key)
                                    }
                                    return next
                                  })
                                }}
                                title={isVisible ? 'Hide value' : 'Show value'}
                              >
                                {isVisible ? (
                                  <EyeOff className="h-4 w-4" />
                                ) : (
                                  <Eye className="h-4 w-4" />
                                )}
                              </Button>
                            )}
                            {hasValue && (!isSensitive || isVisible) && (
                              <CopyButton
                                value={String(value)}
                                minimal
                                className="h-8 w-8 shrink-0"
                              />
                            )}
                          </dd>
                        </div>
                      )
                    }
                  )}
                </dl>
              ) : (
                <div className="text-sm text-muted-foreground">
                  No parameters configured
                </div>
              )}
            </CardContent>
          </Card>

          {/* Backups Section */}
          <Card>
            <CardHeader>
              <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                <div className="space-y-1.5 min-w-0">
                  <CardTitle className="flex items-center gap-2">
                    <span>Backups</span>
                    <Badge variant="outline">
                      {isLoadingBackups ? (
                        <Loader2 className="h-3 w-3 animate-spin" />
                      ) : (
                        serviceBackupsData?.total ?? 0
                      )}
                    </Badge>
                  </CardTitle>
                  <CardDescription>
                    Backups of this service stored across your S3 sources
                  </CardDescription>
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  <Button
                    variant="outline"
                    size="icon"
                    className="h-9 w-9"
                    aria-label="Refresh backups"
                    title="Refresh backups"
                    onClick={() => void refetchBackups()}
                    disabled={isFetchingBackups}
                  >
                    <RefreshCcw
                      className={cn(
                        'h-4 w-4',
                        isFetchingBackups && 'animate-spin',
                      )}
                    />
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    className="gap-2"
                    onClick={() => setIsBackupDialogOpen(true)}
                  >
                    <HardDrive className="h-4 w-4" />
                    <span className="hidden sm:inline">Trigger backup</span>
                    <span className="sm:hidden">Backup</span>
                  </Button>
                </div>
              </div>
            </CardHeader>
            <CardContent>
              {isLoadingBackups ? (
                <div className="flex items-center justify-center py-8">
                  <Loader2 className="h-4 w-4 animate-spin mr-2" />
                  <span className="text-sm text-muted-foreground">
                    Loading backups...
                  </span>
                </div>
              ) : serviceBackups.length === 0 ? (
                <div className="text-sm text-muted-foreground text-center py-8">
                  No backups found for this service yet. Trigger one or
                  configure a schedule from an S3 source.
                </div>
              ) : (
                <ul role="list" className="divide-y divide-border">
                  {paginatedBackups.map((backup) => {
                    const key =
                      backup.backup_id || String(backup.external_service_backup_id)
                    const state = backup.state || 'unknown'
                    const isCompleted = state === 'completed'
                    const isFailed = state === 'failed'
                    const isRunning = state === 'running' || state === 'pending'

                    // Duration only when we have both endpoints. Sub-second
                    // backups render as `<1s` rather than `0s` so the user
                    // sees "yes, it really did finish".
                    const startedMs = new Date(backup.started_at).getTime()
                    const finishedMs = backup.finished_at
                      ? new Date(backup.finished_at).getTime()
                      : null
                    const durationLabel =
                      finishedMs && finishedMs > startedMs
                        ? formatShortDuration(finishedMs - startedMs)
                        : null

                    const linkTo = backup.backup_id
                      ? `/backups/s3-sources/${backup.s3_source_id}/backups/${backup.backup_id}`
                      : `/backups/s3-sources/${backup.s3_source_id}`

                    // All backups in this list belong to the current
                    // service, so the icon is the service type (postgres
                    // -> Database, mongodb -> Leaf, redis -> Server, …).
                    // Using `service.service.service_type` directly keeps
                    // the row in lock-step with the page header even when
                    // the backend hasn't surfaced an `engine` field on
                    // this entry yet (legacy rows).
                    const ServiceIcon = iconForServiceType(
                      service.service.service_type,
                    )

                    return (
                      <li key={key}>
                        <Link
                          to={linkTo}
                          className="flex items-center gap-3 py-3 transition-colors hover:bg-muted/40 focus:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:rounded-md sm:gap-4 -mx-2 px-2"
                        >
                          <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
                            <ServiceIcon className="size-4 text-muted-foreground" />
                          </div>

                          {/* Title + meta. Lead with the human-readable
                              service/source line, secondary line packs the
                              actionable detail (exact time, duration, size,
                              short UUID, error preview). */}
                          <div className="min-w-0 flex-1">
                            <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                              <p className="truncate text-sm font-medium">
                                <TimeAgo date={backup.started_at} />
                              </p>
                              <span
                                className="hidden text-xs text-muted-foreground sm:inline"
                                aria-hidden
                              >
                                ·
                              </span>
                              <span className="font-mono text-xs text-muted-foreground tabular-nums hidden sm:inline">
                                {format(
                                  new Date(backup.started_at),
                                  'MMM d, p',
                                )}
                              </span>
                              {backup.backup_type ? (
                                <Badge variant="outline" className="text-xs">
                                  {backup.backup_type}
                                </Badge>
                              ) : null}
                              {isCompleted ? (
                                <Badge
                                  variant="outline"
                                  className="gap-1 border-emerald-500/50 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400 text-xs"
                                >
                                  <CheckCircle2 className="h-3 w-3" />
                                  Completed
                                </Badge>
                              ) : isRunning ? (
                                <Badge
                                  variant="secondary"
                                  className="gap-1 text-xs"
                                >
                                  <Radio className="h-3 w-3 animate-pulse" />
                                  {state === 'pending' ? 'Pending' : 'Running'}
                                </Badge>
                              ) : isFailed ? (
                                <Badge variant="destructive" className="gap-1 text-xs">
                                  <XCircle className="h-3 w-3" />
                                  Failed
                                </Badge>
                              ) : (
                                <Badge variant="outline" className="text-xs">
                                  {state}
                                </Badge>
                              )}
                            </div>
                            <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground tabular-nums">
                              <span className="truncate">
                                {backup.s3_source_name}
                              </span>
                              {backup.size_bytes && backup.size_bytes > 0 ? (
                                <span className="inline-flex items-center gap-1">
                                  <HardDrive className="h-3 w-3" />
                                  {formatBytes(backup.size_bytes)}
                                </span>
                              ) : null}
                              {durationLabel ? (
                                <span className="inline-flex items-center gap-1">
                                  <Clock className="h-3 w-3" />
                                  {durationLabel}
                                </span>
                              ) : null}
                              {backup.backup_id ? (
                                <span className="font-mono">
                                  #{backup.backup_id.slice(0, 8)}
                                </span>
                              ) : null}
                            </div>
                            {/* Error preview — only when the backup actually
                                failed. Hard-caps the rendered string at ~160
                                chars and clips with CSS `truncate` so a long
                                stack trace can't blow out the card width even
                                if a parent flex container forgets `min-w-0`.
                                Full message is on the BackupDetail page (and
                                surfaced via `title` on hover). */}
                            {isFailed && backup.error_message ? (
                              <p
                                className="mt-1 truncate text-xs text-destructive"
                                title={backup.error_message}
                              >
                                {backup.error_message.length > 160
                                  ? `${backup.error_message.slice(0, 160)}…`
                                  : backup.error_message}
                              </p>
                            ) : null}
                          </div>

                          <Button
                            variant="ghost"
                            size="sm"
                            className="hidden gap-2 sm:flex"
                            tabIndex={-1}
                            asChild={false}
                          >
                            View
                            <ArrowLeft className="h-4 w-4 rotate-180" />
                          </Button>
                        </Link>
                      </li>
                    )
                  })}
                </ul>
              )}
              {backupsTotalPages > 1 && (
                <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between mt-4">
                  <div className="text-sm text-muted-foreground">
                    <span className="hidden sm:inline tabular-nums">
                      Showing {(backupsPage - 1) * BACKUPS_PAGE_SIZE + 1} to{' '}
                      {Math.min(
                        backupsPage * BACKUPS_PAGE_SIZE,
                        serviceBackupsData?.total ?? 0,
                      )}{' '}
                      of {serviceBackupsData?.total ?? 0} backups
                    </span>
                    <span className="sm:hidden tabular-nums">
                      {backupsPage} / {backupsTotalPages}
                    </span>
                  </div>
                  <div className="flex items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setBackupsPage((p) => Math.max(1, p - 1))
                      }
                      disabled={backupsPage === 1}
                    >
                      <ChevronLeft className="h-4 w-4" />
                      <span className="hidden sm:inline">Previous</span>
                    </Button>
                    <div className="hidden sm:flex items-center gap-1">
                      {backupsPageWindow.map((pageNum) => (
                        <Button
                          key={pageNum}
                          variant={
                            pageNum === backupsPage ? 'default' : 'outline'
                          }
                          size="sm"
                          onClick={() => setBackupsPage(pageNum)}
                          className="w-10"
                        >
                          {pageNum}
                        </Button>
                      ))}
                    </div>
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() =>
                        setBackupsPage((p) =>
                          Math.min(backupsTotalPages, p + 1),
                        )
                      }
                      disabled={backupsPage === backupsTotalPages}
                    >
                      <span className="hidden sm:inline">Next</span>
                      <ChevronRight className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
              )}
            </CardContent>
          </Card>

          {/* Environment Variables Section */}
          <Card>
            <CardHeader>
              <CardTitle>Environment Variables</CardTitle>
              <CardDescription>
                Preview of environment variables available to projects using
                this service
              </CardDescription>
            </CardHeader>
            <CardContent>
              {envVarsLoading ? (
                <div className="flex items-center justify-center py-8">
                  <Loader2 className="h-4 w-4 animate-spin mr-2" />
                  <span className="text-sm text-muted-foreground">
                    Loading environment variables...
                  </span>
                </div>
              ) : envVarsError ? (
                <div className="text-center py-8">
                  <AlertCircle className="h-8 w-8 mx-auto text-muted-foreground mb-2" />
                  <p className="text-sm text-muted-foreground">
                    Failed to load environment variables
                  </p>
                </div>
              ) : envVars ? (
                <>
                  <EnvVariablesDisplay
                    variables={envVars}
                    showCopy={true}
                    showMaskToggle={true}
                    defaultMasked={true}
                    maxHeight="20rem"
                  />
                  <p className="text-xs text-muted-foreground text-center mt-3">
                    These variables are automatically available to projects that
                    use this service
                  </p>
                </>
              ) : null}
            </CardContent>
          </Card>

          {isPostgres && pgUpgrades && pgUpgrades.length > 0 ? (
            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <ArrowUpCircle className="h-5 w-5" />
                  Major Version Upgrades
                </CardTitle>
                <CardDescription>
                  History of PostgreSQL major-version upgrades for this service.
                  Click a row to see phase progress and logs.
                </CardDescription>
              </CardHeader>
              <CardContent>
                <div className="space-y-2">
                  {pgUpgrades.map((u) => {
                    const totalPhases = PG_UPGRADE_PHASES.length - 1 // exclude "completed"
                    const pct =
                      u.status === 'completed'
                        ? 100
                        : Math.round(
                            (phaseIndex(u.phase) / totalPhases) * 100,
                          )
                    const statusVariant =
                      u.status === 'completed'
                        ? 'default'
                        : u.status === 'failed'
                        ? 'destructive'
                        : u.status === 'cancelled' ||
                          u.status === 'rolled_back'
                        ? 'secondary'
                        : 'outline'
                    const isActive = !isTerminal(u.status)
                    return (
                      <Link
                        key={u.id}
                        to={`/storage/${id}/upgrades/${u.id}`}
                        className="block rounded-lg border p-3 hover:bg-muted/50 transition-colors"
                      >
                        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                          <div className="flex flex-wrap items-center gap-2 min-w-0">
                            <span className="font-medium text-sm">
                              #{u.id}
                            </span>
                            <span className="text-sm text-muted-foreground truncate">
                              {u.from_version} → {u.to_version}
                            </span>
                            {isActive ? (
                              <Loader2 className="h-3 w-3 animate-spin text-muted-foreground" />
                            ) : null}
                          </div>
                          <div className="flex items-center gap-2">
                            <Badge variant={statusVariant} className="text-xs">
                              {u.status}
                            </Badge>
                            <span className="text-xs text-muted-foreground whitespace-nowrap">
                              <TimeAgo date={u.created_at} />
                            </span>
                          </div>
                        </div>
                        {isActive ? (
                          <div className="mt-2">
                            <div className="flex justify-between text-xs text-muted-foreground mb-1">
                              <span className="truncate">
                                Phase: {u.phase}
                              </span>
                              <span className="whitespace-nowrap ml-2">
                                {pct}%
                              </span>
                            </div>
                            <div className="h-1.5 bg-muted rounded overflow-hidden">
                              <div
                                className="h-full bg-primary transition-all"
                                style={{ width: `${pct}%` }}
                              />
                            </div>
                          </div>
                        ) : u.error_message ? (
                          <p className="mt-2 text-xs text-destructive line-clamp-2">
                            {u.error_message}
                          </p>
                        ) : null}
                      </Link>
                    )
                  })}
                </div>
              </CardContent>
            </Card>
          ) : null}
        </div>
      </div>

      <Dialog open={isStopDialogOpen} onOpenChange={setIsStopDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Stop Service</DialogTitle>
            <DialogDescription>
              Are you sure you want to stop{' '}
              <span className="font-medium text-foreground">
                {service.service.name}
              </span>
              ? All connected projects will lose access to this service until it
              is started again.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setIsStopDialogOpen(false)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={handleStop}
              disabled={stopService.isPending}
            >
              {stopService.isPending && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              Stop Service
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={isDeleteDialogOpen}
        onOpenChange={(open) => {
          setIsDeleteDialogOpen(open)
          if (!open) setDeleteConfirmName('')
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete Service</DialogTitle>
            <DialogDescription>
              This action cannot be undone and all data associated with this
              service will be permanently removed.
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-2">
            <Label htmlFor="confirm-delete-service-name">
              Type{' '}
              <span className="font-mono font-semibold text-foreground">
                {service.service.name}
              </span>{' '}
              to confirm
            </Label>
            <Input
              id="confirm-delete-service-name"
              value={deleteConfirmName}
              onChange={(e) => setDeleteConfirmName(e.target.value)}
              placeholder={service.service.name}
              autoComplete="off"
              autoFocus
            />
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setIsDeleteDialogOpen(false)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={
                deleteService.isPending ||
                deleteConfirmName.trim() !== service.service.name
              }
            >
              {deleteService.isPending && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              Delete
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={!!memberToRemove}
        onOpenChange={(open) => {
          if (!open) setMemberToRemove(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Remove Cluster Member</DialogTitle>
            <DialogDescription>
              Stop and remove{' '}
              <span className="font-mono text-foreground">
                {memberToRemove?.container_name}
              </span>{' '}
              from this cluster. The container, its data volume, and the
              member's DNS record will be deleted. The pg_auto_failover
              monitor will mark the node as unreachable; run{' '}
              <span className="font-mono">pg_autoctl drop node</span>{' '}
              manually if you want a fully-clean monitor view.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setMemberToRemove(null)}
              disabled={removeMember.isPending}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              onClick={() =>
                memberToRemove &&
                removeMember.mutate({
                  serviceId: parseInt(id!),
                  memberId: memberToRemove.id,
                })
              }
              disabled={removeMember.isPending}
            >
              {removeMember.isPending && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              Remove Member
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={!!memberToPromote}
        onOpenChange={(open) => {
          if (!open) setMemberToPromote(null)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Promote to Primary</DialogTitle>
            <DialogDescription>
              Trigger a pg_auto_failover failover so{' '}
              <span className="font-mono text-foreground">
                {memberToPromote?.container_name}
              </span>{' '}
              becomes the new primary. The current primary will be
              demoted to a replica. Brief write unavailability is
              expected during the transition (typically a few seconds).
              The role reconciler refreshes the role-aliased VIP DNS
              records on its next tick (≤30s) so app connections that
              use the FQDN follow without restart.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setMemberToPromote(null)}
              disabled={promoteMember.isPending}
            >
              Cancel
            </Button>
            <Button
              onClick={() =>
                memberToPromote &&
                promoteMember.mutate({
                  serviceId: parseInt(id!),
                  memberId: memberToPromote.id,
                })
              }
              disabled={promoteMember.isPending}
            >
              {promoteMember.isPending && (
                <Loader2 className="h-4 w-4 mr-2 animate-spin" />
              )}
              Promote
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <UpgradeServiceDialog
        open={isUpgradeDialogOpen}
        onOpenChange={setIsUpgradeDialogOpen}
        serviceId={parseInt(id!)}
        serviceName={service.service.name}
        currentImage={service.current_parameters?.docker_image || undefined}
        serviceType={service.service.service_type}
        onSuccess={() => refetch()}
      />

      <MajorUpgradeDialog
        open={isMajorUpgradeDialogOpen}
        onOpenChange={setIsMajorUpgradeDialogOpen}
        serviceId={parseInt(id!)}
        serviceName={service.service.name}
        currentImage={service.current_parameters?.docker_image || ''}
      />

      <EditServiceDialog
        open={isEditDialogOpen}
        onOpenChange={setIsEditDialogOpen}
        service={service.service}
        currentParameters={service.current_parameters}
        onSuccess={() => {
          refetch()
          queryClient.invalidateQueries({
            queryKey: getServiceOptions({
              path: { id: parseInt(id!) },
            }).queryKey,
          })
        }}
      />

      <TriggerBackupDialog
        open={isBackupDialogOpen}
        onOpenChange={setIsBackupDialogOpen}
        serviceId={parseInt(id!)}
        serviceName={service.service.name}
        onSuccess={() => {
          // Reload the service so any status transition (e.g. "backing_up")
          // shows immediately, plus the S3 source list and every per-source
          // backup index so the Backups card picks up the new entry.
          refetch()
          queryClient.invalidateQueries({
            queryKey: getServiceOptions({
              path: { id: parseInt(id!) },
            }).queryKey,
          })
          queryClient.invalidateQueries({
            predicate: (query) => {
              const key = query.queryKey[0] as { _id?: string } | undefined
              return (
                key?._id === 'listSourceBackups' || key?._id === 'listS3Sources'
              )
            },
          })
        }}
      />

    </div>
  )
}
