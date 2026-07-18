import { ExternalServiceInfo, ProjectResponse } from '@/api/client'
import {
  externalServiceMetricsGetRangeOptions,
  linkServiceToProjectMutation,
  listProjectServicesOptions,
  listServicesOptions,
  unlinkServiceFromProjectMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { MetricSparkline } from '@/components/charts/metric-sparkline'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import EmptyStateStorage from '@/components/storage/EmptyStateStorage'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { cn, formatBytes } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ChevronRight,
  Database,
  Info,
  Link2,
  Link2Off,
  MoreHorizontal,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { toast } from 'sonner'

/**
 * Mirrors backend naming conventions in temps-providers/src/externalsvc/.
 */
function getProjectResourcePath(
  serviceType: string,
  projectSlug: string,
  environment = 'production'
): string {
  if (
    serviceType === 's3' ||
    serviceType === 'rustfs' ||
    serviceType === 'minio'
  ) {
    return `${projectSlug}-${environment}`.replace(/_/g, '-').toLowerCase()
  }
  const raw = `${projectSlug}_${environment}`.toLowerCase()
  const normalized = raw.replace(/[^a-z0-9]/g, '_')
  return /^\d/.test(normalized) ? `db_${normalized}` : normalized
}

/**
 * Compact 1h sparkline + current value for one container resource metric of
 * a linked service. Renders nothing when the service has no metric history
 * (monitoring disabled, service stopped) so unmonitored rows stay clean.
 */
function ResourceSparkline({
  serviceId,
  metric,
  label,
  format,
}: {
  serviceId: number
  metric: string
  label: string
  format: (value: number) => string
}) {
  const { data } = useQuery({
    ...externalServiceMetricsGetRangeOptions({
      path: { id: serviceId },
      query: { metric, range: '1h' },
    }),
    staleTime: 30_000,
    refetchInterval: 30_000,
    // Metrics disabled on the service → the endpoint 503s; don't retry-spam.
    retry: false,
  })

  if (!data?.length) return null

  const values = data.map((p) => p.value)
  const last = values[values.length - 1]

  return (
    <div className="hidden w-24 shrink-0 flex-col items-stretch gap-0.5 lg:flex">
      <MetricSparkline data={values} height={16} />
      <span className="text-right text-[10px] tabular-nums text-muted-foreground">
        {label} {format(last)}
      </span>
    </div>
  )
}

function ServiceRow({
  service,
  isLinked,
  isBusy,
  projectSlug,
  onToggle,
}: {
  service: ExternalServiceInfo
  isLinked: boolean
  isBusy: boolean
  projectSlug: string
  onToggle: () => Promise<void>
}) {
  const navigate = useNavigate()

  const primaryHref = isLinked
    ? `/storage/${service.id}/browse?path=${encodeURIComponent(
        getProjectResourcePath(service.service_type, projectSlug)
      )}`
    : `/storage/${service.id}`

  const goToPrimary = () => navigate(primaryHref)

  return (
    <li
      role="button"
      tabIndex={0}
      onClick={goToPrimary}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault()
          goToPrimary()
        }
      }}
      className={cn(
        'group flex items-center gap-4 px-3 py-3',
        'cursor-pointer transition-colors',
        'hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none'
      )}
    >
      <div className="shrink-0">
        <ServiceLogo service={service.service_type} />
      </div>

      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <p className="truncate text-sm font-medium text-foreground">
            {service.name}
          </p>
          {isLinked ? (
            <Badge
              variant="outline"
              className="shrink-0 border-emerald-600/20 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400"
            >
              Linked
            </Badge>
          ) : null}
        </div>
        <p className="mt-0.5 truncate text-xs text-muted-foreground">
          {service.service_type}
          {isLinked
            ? ` · ${getProjectResourcePath(service.service_type, projectSlug)}`
            : ''}
        </p>
      </div>

      {isLinked ? (
        <>
          <ResourceSparkline
            serviceId={service.id}
            metric="container.cpu_percent"
            label="CPU"
            format={(v) => `${v.toFixed(1)}%`}
          />
          <ResourceSparkline
            serviceId={service.id}
            metric="container.memory_used_bytes"
            label="Mem"
            format={formatBytes}
          />
        </>
      ) : null}

      <div
        className="flex items-center gap-1"
        onClick={(e) => e.stopPropagation()}
      >
        {isLinked ? (
          <Button
            variant="ghost"
            size="sm"
            className="h-8 gap-2 text-muted-foreground hover:text-foreground"
            onClick={(e) => {
              e.stopPropagation()
              goToPrimary()
            }}
          >
            <Database className="size-3.5" />
            <span className="hidden sm:inline">Browse</span>
          </Button>
        ) : (
          <Button
            variant="outline"
            size="sm"
            className="h-8"
            disabled={isBusy}
            onClick={(e) => {
              e.stopPropagation()
              void onToggle()
            }}
          >
            <Link2 className="size-3.5" />
            Link
          </Button>
        )}

        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              size="sm"
              className="relative size-8 p-0 text-muted-foreground hover:text-foreground"
              aria-label={`Actions for ${service.name}`}
              onClick={(e) => e.stopPropagation()}
            >
              <MoreHorizontal className="size-4" />
              <span
                aria-hidden="true"
                className="absolute top-1/2 left-1/2 size-[max(100%,3rem)] -translate-1/2 pointer-fine:hidden"
              />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-48">
            <DropdownMenuItem
              onSelect={() => navigate(`/storage/${service.id}`)}
            >
              View details
            </DropdownMenuItem>
            {isLinked ? (
              <DropdownMenuItem
                onSelect={() =>
                  navigate(
                    `/storage/${service.id}/browse?path=${encodeURIComponent(
                      getProjectResourcePath(service.service_type, projectSlug)
                    )}`
                  )
                }
              >
                Browse data
              </DropdownMenuItem>
            ) : null}
            <DropdownMenuSeparator />
            {isLinked ? (
              <DropdownMenuItem
                disabled={isBusy}
                className="text-destructive focus:text-destructive focus:bg-destructive/10"
                onSelect={() => void onToggle()}
              >
                <Link2Off className="size-3.5" />
                Unlink from project
              </DropdownMenuItem>
            ) : (
              <DropdownMenuItem
                disabled={isBusy}
                onSelect={() => void onToggle()}
              >
                <Link2 className="size-3.5" />
                Link to project
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>

        <ChevronRight
          className="size-4 text-muted-foreground/60 transition-transform group-hover:translate-x-0.5 group-hover:text-muted-foreground"
          aria-hidden="true"
        />
      </div>
    </li>
  )
}

export function ProjectStorage({ project }: { project: ProjectResponse }) {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [isCreateDropdownOpen, setIsCreateDropdownOpen] = useState(false)

  useEffect(() => {
    setBreadcrumbs([{ label: 'Databases' }])
  }, [setBreadcrumbs])

  useKeyboardShortcut({
    key: 'n',
    callback: () => setIsCreateDropdownOpen(true),
  })

  const {
    data: services,
    isLoading: isLoadingServices,
    refetch: refetchServices,
  } = useQuery({
    ...listServicesOptions(),
  })

  const { data: servicesLinked, refetch: refetchServicesLinked } = useQuery({
    ...listProjectServicesOptions({
      path: { project_id: project.id },
    }),
  })

  const linkServiceMutation = useMutation({
    ...linkServiceToProjectMutation(),
    meta: { errorTitle: 'Failed to link service to project' },
    onSuccess: () => refetchServicesLinked(),
  })

  const unlinkServiceMutation = useMutation({
    ...unlinkServiceFromProjectMutation(),
    meta: { errorTitle: 'Failed to unlink service from project' },
    onSuccess: () => refetchServicesLinked(),
  })

  const handleServiceToggle = async (serviceId: number) => {
    const isLinked = servicesLinked?.some((s) => s.service.id === serviceId)

    if (isLinked) {
      const promise = unlinkServiceMutation.mutateAsync({
        path: { id: serviceId, project_id: project.id },
      })
      toast.promise(promise, {
        loading: 'Unlinking service...',
        success: 'Service unlinked',
        error: 'Failed to unlink service',
      })
      await promise.catch(() => {})
    } else {
      const promise = linkServiceMutation.mutateAsync({
        path: { id: serviceId },
        body: { project_id: project.id },
      })
      toast.promise(promise, {
        loading: 'Linking service...',
        success: 'Service linked',
        error: 'Failed to link service',
      })
      await promise.catch(() => {})
    }

    await refetchServicesLinked()
  }

  const linkedCount = servicesLinked?.length ?? 0
  const totalCount = services?.length ?? 0
  const isToggling =
    linkServiceMutation.isPending || unlinkServiceMutation.isPending

  const header = (
    <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
      <div>
        <h1 className="text-xl font-semibold tracking-tight sm:text-2xl">
          Databases
        </h1>
        <p className="mt-1 text-sm text-muted-foreground">
          Link Postgres, MongoDB, Redis, or S3-compatible services to this
          project.
          {totalCount > 0 ? (
            <span className="ml-1 tabular-nums">
              {linkedCount} of {totalCount} linked.
            </span>
          ) : null}
        </p>
      </div>
      <CreateServiceButton
        open={isCreateDropdownOpen}
        onOpenChange={setIsCreateDropdownOpen}
        onSuccess={() => {
          refetchServices()
          refetchServicesLinked()
        }}
      />
    </div>
  )

  if (isLoadingServices) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 p-4 md:p-6">
          {header}
          <ul
            role="list"
            className="divide-y divide-border rounded-lg border border-border"
          >
            {[...Array(4)].map((_, i) => (
              <li key={i} className="flex items-center gap-4 px-3 py-3">
                <div className="size-8 shrink-0 animate-pulse rounded-md bg-muted" />
                <div className="flex-1 space-y-2">
                  <div className="h-4 w-1/3 animate-pulse rounded bg-muted" />
                  <div className="h-3 w-1/5 animate-pulse rounded bg-muted" />
                </div>
                <div className="h-8 w-20 animate-pulse rounded bg-muted" />
              </li>
            ))}
          </ul>
        </div>
      </div>
    )
  }

  if (!services?.length) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6 p-4 md:p-6">
          {header}
          <Alert>
            <Info className="h-4 w-4" />
            <AlertTitle>How databases work in Temps</AlertTitle>
            <AlertDescription className="text-muted-foreground">
              One service (e.g. Postgres) hosts databases for{' '}
              <strong>all your projects</strong>. When you link a service here,
              Temps automatically creates a dedicated{' '}
              <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
                {`<project>_<env>`}
              </code>{' '}
              database for each environment — you don&apos;t need a separate
              Postgres for every project (unlike Heroku/Render).
            </AlertDescription>
          </Alert>
          <EmptyStateStorage />
        </div>
      </div>
    )
  }

  const linkedServices = services.filter((s) =>
    servicesLinked?.some((l) => l.service.id === s.id)
  )
  const availableServices = services.filter(
    (s) => !servicesLinked?.some((l) => l.service.id === s.id)
  )

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-8 p-4 md:p-6">
        {header}

        <Alert>
          <Info className="h-4 w-4" />
          <AlertTitle>How databases work in Temps</AlertTitle>
          <AlertDescription className="text-muted-foreground">
            One service (e.g. Postgres) hosts databases for{' '}
            <strong>all your projects</strong>. When you link a service here,
            Temps automatically creates a dedicated{' '}
            <code className="rounded bg-muted px-1 py-0.5 font-mono text-xs">
              {`<project>_<env>`}
            </code>{' '}
            database for each environment — you don&apos;t need a separate
            Postgres for every project (unlike Heroku/Render).
          </AlertDescription>
        </Alert>

        {linkedServices.length > 0 ? (
          <section>
            <div className="mb-2 flex items-baseline justify-between">
              <h2 className="text-sm font-medium text-foreground">Linked</h2>
              <span className="text-xs text-muted-foreground tabular-nums">
                {linkedServices.length}
              </span>
            </div>
            <ul
              role="list"
              className="divide-y divide-border rounded-lg border border-border overflow-hidden"
            >
              {linkedServices.map((service) => (
                <ServiceRow
                  key={service.id}
                  service={service}
                  isLinked
                  isBusy={isToggling}
                  projectSlug={project.slug}
                  onToggle={() => handleServiceToggle(service.id)}
                />
              ))}
            </ul>
          </section>
        ) : null}

        {availableServices.length > 0 ? (
          <section>
            <div className="mb-2 flex items-baseline justify-between">
              <h2 className="text-sm font-medium text-foreground">Available</h2>
              <span className="text-xs text-muted-foreground tabular-nums">
                {availableServices.length}
              </span>
            </div>
            <ul
              role="list"
              className="divide-y divide-border rounded-lg border border-border overflow-hidden"
            >
              {availableServices.map((service) => (
                <ServiceRow
                  key={service.id}
                  service={service}
                  isLinked={false}
                  isBusy={isToggling}
                  projectSlug={project.slug}
                  onToggle={() => handleServiceToggle(service.id)}
                />
              ))}
            </ul>
          </section>
        ) : null}
      </div>
    </div>
  )
}
