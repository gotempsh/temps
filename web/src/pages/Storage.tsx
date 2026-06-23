import {
  getServiceRuntimeOptions,
  getServiceStatsOptions,
  listServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ExternalServiceInfo } from '@/api/client/types.gen'
import {
  listServiceHealthStatuses,
  type HealthStatus,
} from '@/lib/service-health'
import { formatBytes } from '@/lib/utils'
import { formatCpuUsage } from '@/lib/cpu-format'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import { DeleteServiceButton } from '@/components/storage/DeleteServiceButton'
import { EditServiceDialog } from '@/components/storage/EditServiceDialog'
import { ImportServiceButton } from '@/components/storage/ImportServiceButton'
import { PlatformServices } from '@/components/storage/PlatformServices'
import EmptyStateStorage from '@/components/storage/EmptyStateStorage'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useQuery } from '@tanstack/react-query'
import {
  Cpu,
  Database,
  HardDrive,
  MemoryStick,
  Pencil,
  RefreshCcw,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { useEffect, useState } from 'react'
import { useNavigate, useSearchParams } from 'react-router-dom'
import { TimeAgo } from '@/components/utils/TimeAgo'

export function Storage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const navigate = useNavigate()
  const [searchParams, setSearchParams] = useSearchParams()
  const [isEditDialogOpen, setIsEditDialogOpen] = useState(false)
  const [selectedService, setSelectedService] = useState<ExternalServiceInfo | null>(null)
  const [isCreateDropdownOpen, setIsCreateDropdownOpen] = useState(false)

  // Get active tab from URL or default to 'external'
  const activeTab = searchParams.get('tab') || 'external'

  const handleTabChange = (value: string) => {
    setSearchParams({ tab: value })
  }

  const {
    data: services,
    isLoading,
    error,
    refetch,
  } = useQuery({
    ...listServicesOptions(),
  })

  // Batch-fetch current health for every service shown so each row can render
  // a dot without firing N requests. Re-polls every 30s to match the backend
  // probe cadence.
  const serviceIds = (services ?? []).map((s) => s.id)
  const idsKey = serviceIds.join(',')
  const { data: healthMap } = useQuery({
    queryKey: ['service-health-batch', idsKey],
    queryFn: () => listServiceHealthStatuses(serviceIds),
    enabled: serviceIds.length > 0,
    refetchInterval: 30_000,
    staleTime: 25_000,
  })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Databases', href: '/storage' }])
  }, [setBreadcrumbs])

  // Keyboard shortcut: N to open the create service dropdown
  useKeyboardShortcut({
    key: 'n',
    callback: () => setIsCreateDropdownOpen(true),
  })

  usePageTitle('Databases')

  // Render external services content based on loading/error/empty state
  const renderExternalServicesContent = () => {
    if (isLoading) {
      return (
        <div className="space-y-4">
          <div className="flex items-center justify-end">
            <div className="h-9 w-24 bg-muted rounded animate-pulse" />
          </div>
          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
            {[...Array(6)].map((_, i) => (
              <Card key={i} className="p-4">
                <div className="flex items-start gap-3">
                  <div className="size-11 shrink-0 rounded-md bg-muted animate-pulse" />
                  <div className="flex-1 space-y-2 pt-0.5">
                    <div className="h-4 w-32 bg-muted rounded animate-pulse" />
                    <div className="h-3 w-20 bg-muted rounded animate-pulse" />
                  </div>
                </div>
                <div className="mt-4 h-3 w-full bg-muted rounded animate-pulse" />
              </Card>
            ))}
          </div>
        </div>
      )
    }

    if (error) {
      return (
        <div className="flex flex-col items-center justify-center py-12 text-center">
          <p className="text-sm text-muted-foreground mb-4">
            Failed to load services
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
      )
    }

    if (!services?.length) {
      return <EmptyStateStorage />
    }

    return (
      <>
        <div className="flex items-center justify-end mb-4">
          <div className="flex items-center gap-2">
            <ImportServiceButton onSuccess={() => refetch()} />
            <CreateServiceButton
              onSuccess={() => refetch()}
              open={isCreateDropdownOpen}
              onOpenChange={setIsCreateDropdownOpen}
            />
          </div>
        </div>

        <ServicesCardGrid
          services={services}
          healthMap={healthMap}
          onOpen={(id) => navigate(`/storage/${id}`)}
          onEdit={(service) => {
            setSelectedService(service)
            setIsEditDialogOpen(true)
          }}
          onDeleteSuccess={() => refetch()}
        />
      </>
    )
  }

  return (
    <div className="flex-1 overflow-auto">
      <div className="sm:p-4 space-y-6 md:p-6">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-semibold sm:text-2xl">Databases</h1>
        </div>

        <Tabs value={activeTab} onValueChange={handleTabChange} className="space-y-6">
          <TabsList>
            <TabsTrigger value="platform" className="gap-2">
              <Database className="h-4 w-4" />
              Platform Services
            </TabsTrigger>
            <TabsTrigger value="external" className="gap-2">
              <HardDrive className="h-4 w-4" />
              External Services
            </TabsTrigger>
          </TabsList>

          <TabsContent value="platform" className="space-y-6">
            <PlatformServices />
          </TabsContent>

          <TabsContent value="external" className="space-y-6">
            {renderExternalServicesContent()}
          </TabsContent>
        </Tabs>

        {selectedService && (
          <EditServiceDialog
            open={isEditDialogOpen}
            onOpenChange={(open) => {
              setIsEditDialogOpen(open)
              if (!open) {
                setSelectedService(null)
              }
            }}
            service={selectedService}
            onSuccess={() => {
              setIsEditDialogOpen(false)
              setSelectedService(null)
              refetch()
            }}
          />
        )}
      </div>
    </div>
  )
}

// ── Shared variant helpers ──

interface ServicesVariantProps {
  services: ExternalServiceInfo[]
  healthMap?: Map<number, { status?: HealthStatus | null }>
  onOpen: (id: number) => void
  onEdit: (service: ExternalServiceInfo) => void
  onDeleteSuccess: () => void
}

/**
 * Small solid dot reflecting the latest health probe
 * (green=operational, amber=degraded, red=down). We render nothing when the
 * service hasn't been probed yet so the row doesn't flash a placeholder.
 */
function HealthDot({
  status,
  className,
}: {
  status: HealthStatus | null | undefined
  className?: string
}) {
  if (!status) return null
  const tone =
    status === 'operational'
      ? 'bg-green-500'
      : status === 'degraded'
        ? 'bg-amber-500'
        : 'bg-red-500'
  const label =
    status === 'operational'
      ? 'Operational'
      : status === 'degraded'
        ? 'Degraded'
        : 'Down'
  return (
    <span
      className={cn('inline-block size-2 rounded-full', tone, className)}
      title={label}
      aria-label={label}
    />
  )
}

function ServiceStatusDot({ status }: { status: string }) {
  const tone =
    status === 'running' || status === 'active' || status === 'ready'
      ? 'bg-success'
      : status === 'error' || status === 'failed'
        ? 'bg-destructive'
        : status === 'pending' || status === 'initializing'
          ? 'bg-warning'
          : 'bg-muted-foreground/40'
  return (
    <span className="flex items-center gap-1.5">
      <span className={cn('size-1.5 rounded-full', tone)} />
      <span className="text-xs capitalize text-muted-foreground">
        {status || 'unknown'}
      </span>
    </span>
  )
}

function ServiceActions({
  service,
  onEdit,
  onDeleteSuccess,
}: {
  service: ExternalServiceInfo
  onEdit: (service: ExternalServiceInfo) => void
  onDeleteSuccess: () => void
}) {
  return (
    <div
      className="flex items-center gap-1"
      onClick={(e) => e.stopPropagation()}
    >
      <Button
        variant="ghost"
        size="icon"
        onClick={() => onEdit(service)}
        className="size-8"
      >
        <Pencil className="size-3.5" />
      </Button>
      <DeleteServiceButton
        serviceId={service.id}
        serviceName={service.name}
        onSuccess={onDeleteSuccess}
      />
    </div>
  )
}

// ── Variant: Card grid (scan all services at a glance) ──

function ServicesCardGrid({
  services,
  healthMap,
  onOpen,
  onEdit,
  onDeleteSuccess,
}: ServicesVariantProps) {
  return (
    <div
      role="list"
      className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3"
    >
      {services.map((service) => {
        // Only show the health dot for services the backend is actually
        // probing (status === 'running'). Others haven't been checked.
        const health =
          service.status === 'running'
            ? healthMap?.get(service.id)?.status ?? null
            : null
        return (
          <ServiceCard
            key={service.id}
            service={service}
            health={health}
            onOpen={onOpen}
            onEdit={onEdit}
            onDeleteSuccess={onDeleteSuccess}
          />
        )
      })}
    </div>
  )
}

function ServiceCard({
  service,
  health,
  onOpen,
  onEdit,
  onDeleteSuccess,
}: {
  service: ExternalServiceInfo
  health: HealthStatus | null
  onOpen: (id: number) => void
  onEdit: (service: ExternalServiceInfo) => void
  onDeleteSuccess: () => void
}) {
  return (
    <Card
      role="listitem"
      onClick={() => onOpen(service.id)}
      className="group relative flex cursor-pointer flex-col gap-4 p-4 hover:border-foreground/20 hover:shadow-md"
    >
      <div className="flex items-start gap-3">
        {/*
          Wrap the logo so we can anchor the health dot in its bottom-right
          corner (Vercel deployment-status pattern).
        */}
        <div className="relative shrink-0">
          <div className="flex size-11 items-center justify-center rounded-md border bg-background">
            <ServiceLogo service={service.service_type} className="size-6" />
          </div>
          {health ? (
            <HealthDot
              status={health}
              className="absolute -bottom-0.5 -right-0.5 ring-2 ring-card"
            />
          ) : null}
        </div>

        <div className="min-w-0 flex-1">
          <p className="truncate text-base font-medium sm:text-sm">
            {service.name}
          </p>
          <div className="mt-1 flex flex-wrap items-center gap-1.5">
            <span className="rounded border bg-muted/50 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-muted-foreground">
              {service.service_type}
            </span>
            {service.topology === 'cluster' && (
              <span className="rounded border px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">
                Cluster
              </span>
            )}
            {service.version && (
              <span className="font-mono text-[10px] text-muted-foreground">
                v{service.version}
              </span>
            )}
          </div>
        </div>

        {/*
          Touch devices have no hover, so keep actions visible there; on
          pointer/larger screens they fade in on hover (or keyboard focus)
          to keep the card header tidy.
        */}
        <div className="shrink-0 sm:opacity-0 sm:transition-opacity sm:group-hover:opacity-100 sm:group-focus-within:opacity-100">
          <ServiceActions
            service={service}
            onEdit={onEdit}
            onDeleteSuccess={onDeleteSuccess}
          />
        </div>
      </div>

      <div className="mt-auto flex items-center justify-between gap-3 border-t pt-3 text-xs text-muted-foreground">
        <div className="flex min-w-0 items-center gap-3">
          <ServiceStatusDot status={service.status} />
          <span className="truncate">
            Created <TimeAgo date={service.created_at} />
          </span>
        </div>
        {service.status === 'running' && (
          <ServiceMetricsCell serviceId={service.id} />
        )}
      </div>
    </Card>
  )
}

/**
 * Compact CPU + memory readout (with applied caps) for one running service.
 *
 * Aggregates across all cluster members — sum of CPU% (rebased against the
 * member's CPU cap so a "1 of 4 cores" cap reads 100% at saturation, not
 * 25%) and sum of memory bytes / sum of memory caps.
 *
 * Two queries: stats polls every 10s (live), runtime polls every 60s
 * (limits change rarely). The detail page polls faster — 10s is fine for
 * "is this database loaded?" from a list view.
 */
function ServiceMetricsCell({ serviceId }: { serviceId: number }) {
  const stats = useQuery({
    ...getServiceStatsOptions({ path: { id: serviceId } }),
    refetchInterval: 10_000,
    refetchIntervalInBackground: false,
    staleTime: 8_000,
  })
  const runtime = useQuery({
    ...getServiceRuntimeOptions({ path: { id: serviceId } }),
    refetchInterval: 60_000,
    staleTime: 50_000,
  })

  if (
    stats.isPending ||
    stats.isError ||
    !stats.data?.members?.length
  ) {
    return null
  }

  // Sum raw Docker CPU% across members (200% per member = 2 cores). We
  // render as cores used (% / 100), which sums cleanly across heterogeneous
  // caps — "0.5 + 2.0 = 2.5 cores used" beats trying to reconcile mixed
  // "% of cap" numbers.
  const cpuPercentTotal = stats.data.members.reduce(
    (sum, m) => sum + (m.cpu_percent ?? 0),
    0,
  )

  const memBytes = stats.data.members.reduce(
    (sum, m) => sum + (m.memory_usage_bytes ?? 0),
    0,
  )
  const hasCpu = stats.data.members.some((m) => m.cpu_percent != null)
  const hasMem = stats.data.members.some((m) => m.memory_usage_bytes != null)

  // Sum applied caps across members. memory_mb is in MiB; convert to bytes
  // so it composes with formatBytes. CPU cap in cores (sum across members).
  const memLimitMib = runtime.data?.members.reduce(
    (sum, r) => sum + (r.resource_limits?.memory_mb ?? 0),
    0,
  )
  const memLimitBytes =
    memLimitMib != null && memLimitMib > 0
      ? memLimitMib * 1024 * 1024
      : null
  const cpuLimitCores = runtime.data?.members.reduce((sum, r) => {
    const nano = r.resource_limits?.nano_cpus ?? 0
    return sum + (nano > 0 ? nano / 1_000_000_000 : 0)
  }, 0)
  const hasCpuCap = cpuLimitCores != null && cpuLimitCores > 0
  const hasMemCap = memLimitBytes != null && memLimitBytes > 0

  if (!hasCpu && !hasMem) return null

  return (
    <div className="flex shrink-0 items-center gap-3 text-xs text-muted-foreground">
      {hasCpu && (
        <span
          className="flex items-center gap-1 tabular-nums"
          title={
            hasCpuCap
              ? `CPU usage of ${cpuLimitCores!.toFixed(cpuLimitCores! % 1 === 0 ? 0 : 2)} core cap`
              : 'CPU usage (uncapped)'
          }
        >
          <Cpu className="size-3.5" />
          <span>
            {formatCpuUsage(cpuPercentTotal, hasCpuCap ? cpuLimitCores! : null)}
          </span>
        </span>
      )}
      {hasMem && (
        <span
          className="flex items-center gap-1 tabular-nums"
          title={
            hasMemCap
              ? `Memory usage of ${formatBytes(memLimitBytes!, 0)} cap`
              : 'Memory usage (uncapped)'
          }
        >
          <MemoryStick className="size-3.5" />
          <span>{formatBytes(memBytes, 1)}</span>
          {hasMemCap && (
            <span className="text-muted-foreground/60">
              / {formatBytes(memLimitBytes!, 0)}
            </span>
          )}
        </span>
      )}
    </div>
  )
}
