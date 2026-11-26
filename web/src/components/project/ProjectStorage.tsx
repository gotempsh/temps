// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader } from '@/components/layout/PageContainer'
import {
  ExternalServiceInfo,
  ProjectResponse,
  ProjectServiceInfo,
} from '@/api/client'
import {
  externalServiceMetricsGetRangeOptions,
  linkServiceToProjectMutation,
  listProjectServicesOptions,
  unlinkServiceFromProjectMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { MetricSparkline } from '@/components/charts/metric-sparkline'
import { useIsDesktop } from '@/components/hooks/use-mobile'
import { CreateServiceButton } from '@/components/storage/CreateServiceButton'
import { Input } from '@/components/ui/input'
import {
  DatabaseProvisioningDialog,
  type DatabaseProvisioningSelection,
} from '@/components/storage/DatabaseProvisioningDialog'
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
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { serverDatabasesQuery } from '@/lib/server-databases'
import { cn, formatBytes } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ChevronRight,
  Database,
  HardDrive,
  Link2,
  Link2Off,
  MoreHorizontal,
  RotateCcw,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { DatabaseTypeCards } from '@/components/storage/DatabaseTypeCards'
import type { ServiceLinkChange } from '@/lib/service-link-redeploy'
import { ServiceLinkRedeployPrompt } from './ServiceLinkRedeployPrompt'
import { projectServiceResourcePath } from '@/lib/database-provisioning'
import { linkableDatabases } from '@/lib/database-links'
import {
  CONTAINER_CPU_METRIC,
  containerCpuChartMetric,
  containerCpuHistoryMetric,
  withCpuHistory,
} from '@/lib/container-cpu'
import { formatCpuUsage } from '@/lib/cpu-format'
import { serviceBackupsHref } from '@/lib/service-backup-setup'
import { toast } from 'sonner'

type ResourceSparklineProps = {
  serviceId: number
  metric: string
  /** Charted before `metric` has readings; see `withCpuHistory`. */
  historyMetric?: string
  label: string
  format: (value: number) => string
}

/**
 * Compact 1h sparkline + current value for one container resource metric of
 * a linked service. Renders nothing when the service has no metric history
 * (monitoring disabled, service stopped) so unmonitored rows stay clean.
 * A narrow row has no room for it: below `lg` it is not mounted, so it
 * polls nothing there.
 */
export function ResourceSparkline(props: ResourceSparklineProps) {
  const wide = useIsDesktop()
  return wide ? <ResourceSparklineChart {...props} /> : null
}

function ResourceSparklineChart({
  serviceId,
  metric,
  historyMetric,
  label,
  format,
}: ResourceSparklineProps) {
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
  const { data: historyData } = useQuery({
    ...externalServiceMetricsGetRangeOptions({
      path: { id: serviceId },
      query: { metric: historyMetric ?? metric, range: '1h' },
    }),
    enabled: historyMetric != null,
    staleTime: 30_000,
    refetchInterval: 30_000,
    retry: false,
  })

  const points = historyMetric
    ? withCpuHistory(data ?? [], historyData ?? [])
    : (data ?? [])
  // Trend glyph only: buckets nothing was sampled in (`null`) are dropped.
  const values = points
    .map((p) => p.value)
    .filter((v): v is number => v != null && Number.isFinite(v))
  if (values.length === 0) return null
  const last = values[values.length - 1]

  return (
    <div className="flex w-24 shrink-0 flex-col items-stretch gap-0.5">
      <MetricSparkline data={values} height={16} />
      <span className="text-right text-[10px] tabular-nums text-muted-foreground">
        {label} {format(last)}
      </span>
    </div>
  )
}

export function ServiceRow({
  service,
  isLinked,
  isBusy,
  projectSlug,
  onToggle,
  link,
}: {
  service: ExternalServiceInfo
  isLinked: boolean
  isBusy: boolean
  projectSlug: string
  onToggle: () => Promise<void>
  link?: ProjectServiceInfo
}) {
  const navigate = useNavigate()

  const primaryHref = isLinked
    ? `/storage/${service.id}/browse?path=${encodeURIComponent(
        projectServiceResourcePath(
          service.service_type,
          projectSlug,
          'production',
          link
        )
      )}`
    : `/storage/${service.id}`

  const goToPrimary = () => navigate(primaryHref)
  // A cluster's `container.cpu_percent` is one series per member, which a
  // range query averages; read its busiest member, as its service page does.
  const cpuMetric = containerCpuChartMetric(
    CONTAINER_CPU_METRIC,
    service.topology === 'cluster'
  )

  return (
    <li
      role="button"
      tabIndex={0}
      onClick={goToPrimary}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return
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
          {isLinked &&
            ['postgres', 'mariadb', 'mongodb'].includes(
              service.service_type
            ) && (
              <span>
                {' '}
                ·{' '}
                {link?.database_provisioning_mode === 'custom'
                  ? 'Custom database'
                  : link?.database_provisioning_mode === 'project'
                    ? 'Per service'
                    : 'Per environment'}
              </span>
            )}
          {isLinked
            ? ` · ${projectServiceResourcePath(service.service_type, projectSlug, 'production', link)}`
            : ''}
        </p>
      </div>

      {isLinked ? (
        <>
          <ResourceSparkline
            serviceId={service.id}
            metric={cpuMetric}
            historyMetric={containerCpuHistoryMetric(
              CONTAINER_CPU_METRIC,
              cpuMetric
            )}
            label="CPU"
            format={(v) => formatCpuUsage(v, null)}
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
            <span className="max-sm:sr-only">Browse</span>
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
                      projectServiceResourcePath(
                        service.service_type,
                        projectSlug,
                        'production',
                        link
                      )
                    )}`
                  )
                }
              >
                Browse data
              </DropdownMenuItem>
            ) : null}
            {isLinked ? (
              <>
                <DropdownMenuItem
                  onSelect={() => navigate(serviceBackupsHref(service.id))}
                >
                  <HardDrive className="size-3.5" />
                  Backups
                </DropdownMenuItem>
                <DropdownMenuItem
                  onSelect={() => navigate(`/storage/${service.id}/restore`)}
                >
                  <RotateCcw className="size-3.5" />
                  Restore…
                </DropdownMenuItem>
              </>
            ) : null}
            <DropdownMenuSeparator />
            {isLinked ? (
              <DropdownMenuItem
                disabled={isBusy}
                className="text-destructive focus:text-destructive focus:bg-destructive/10"
                onSelect={() => void onToggle()}
              >
                <Link2Off className="size-3.5" />
                Unlink from service
              </DropdownMenuItem>
            ) : (
              <DropdownMenuItem
                disabled={isBusy}
                onSelect={() => void onToggle()}
              >
                <Link2 className="size-3.5" />
                Link to service
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

/** Databases listed to link before the rest are found by search. */
const LINKABLE_ROWS = 6

export function ProjectStorage({ project }: { project: ProjectResponse }) {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [isCreateDropdownOpen, setIsCreateDropdownOpen] = useState(false)
  const [search, setSearch] = useState('')
  // The server-wide list is loaded only when the operator asks to link an
  // existing database: by default this page shows this service's own.
  const [servicePendingLink, setServicePendingLink] =
    useState<ExternalServiceInfo | null>(null)
  // The last link change, until the operator redeploys or dismisses it: the
  // running app only sees the new variables after its next deployment.
  const [linkChange, setLinkChange] = useState<ServiceLinkChange | null>(null)
  const [serviceToUnlink, setServiceToUnlink] =
    useState<ExternalServiceInfo | null>(null)

  useEffect(() => {
    setBreadcrumbs([{ label: 'Databases' }])
  }, [setBreadcrumbs])

  useKeyboardShortcut({
    key: 'n',
    callback: () => setIsCreateDropdownOpen(true),
  })

  const {
    data: services,
    refetch: refetchServices,
    isError: servicesError,
  } = useQuery({
    // Asked for with the page: it decides whether linking is offered.
    ...serverDatabasesQuery(),
    retry: false,
  })

  const {
    data: servicesLinked,
    refetch: refetchServicesLinked,
    isLoading: loadingLinks,
    isError: linksError,
  } = useQuery({
    retry: false,
    ...listProjectServicesOptions({
      path: { project_id: project.id },
    }),
  })

  const linkServiceMutation = useMutation({
    ...linkServiceToProjectMutation(),
    meta: { errorTitle: 'Failed to link database to service' },
    onSuccess: () => refetchServicesLinked(),
  })

  const unlinkServiceMutation = useMutation({
    ...unlinkServiceFromProjectMutation(),
    meta: { errorTitle: 'Failed to unlink database from service' },
    onSuccess: () => refetchServicesLinked(),
  })

  const serviceName = (serviceId: number) =>
    servicesLinked?.find((link) => link.service.id === serviceId)?.service
      .name ??
    services?.find((item) => item.id === serviceId)?.name ??
    'The service'

  const linkService = async (
    serviceId: number,
    selection?: DatabaseProvisioningSelection
  ) => {
    const promise = linkServiceMutation.mutateAsync({
      path: { id: serviceId },
      body: { project_id: project.id, ...selection },
    })
    toast.promise(promise, {
      loading: 'Linking service...',
      success: 'Service linked',
    })
    await promise
    setLinkChange({ kind: 'linked', serviceName: serviceName(serviceId) })
    await refetchServicesLinked()
  }

  const unlinkService = async (serviceId: number) => {
    const promise = unlinkServiceMutation.mutateAsync({
      path: { id: serviceId, project_id: project.id },
    })
    toast.promise(promise, {
      loading: 'Unlinking service...',
      success: 'Service unlinked',
    })
    await promise.then(
      () =>
        setLinkChange({
          kind: 'unlinked',
          serviceName: serviceName(serviceId),
        }),
      () => {}
    )
    await refetchServicesLinked()
  }

  const handleServiceToggle = async (serviceId: number) => {
    const linked = servicesLinked?.find((s) => s.service.id === serviceId)

    if (linked) {
      // Unlinking is confirmed first; the request is sent from the dialog.
      setServiceToUnlink(linked.service)
      return
    }

    const service = services?.find((item) => item.id === serviceId)
    if (
      service &&
      ['postgres', 'mariadb', 'mongodb'].includes(service.service_type)
    ) {
      setServicePendingLink(service)
      return
    }
    await linkService(serviceId).catch(() => {})

    await refetchServicesLinked()
  }

  const linkedCount = servicesLinked?.length ?? 0
  const isToggling =
    linkServiceMutation.isPending || unlinkServiceMutation.isPending

  const databaseTypes = (
    <DatabaseTypeCards
      projectId={project.id}
      heading={linkedCount ? 'Create a new database' : 'Create a database'}
      description={`Choose a database type to configure a new database linked to ${project.name}.`}
    />
  )

  const header = (
    <PageHeader
      title="Databases"
      description={
        <span className="tabular-nums">
          {linkedCount === 1
            ? '1 database linked to this service.'
            : `${linkedCount} databases linked to this service.`}
        </span>
      }
      actions={
        <div className="flex flex-wrap items-center gap-2">
          <Button asChild variant="outline" size="sm">
            <Link to="/storage" title="Every database on this server">
              All databases
            </Link>
          </Button>
          <CreateServiceButton
            projectId={project.id}
            label="Create database"
            open={isCreateDropdownOpen}
            onOpenChange={setIsCreateDropdownOpen}
          />
        </div>
      }
    />
  )

  if (linksError) {
    return (
      <div className="space-y-6">
        {header}
        <Alert variant="destructive">
          <AlertTitle>Databases could not be loaded</AlertTitle>
          <AlertDescription>
            Could not determine which databases are linked to this service.
            <Button
              variant="outline"
              size="sm"
              className="ml-3"
              onClick={() => void refetchServicesLinked()}
            >
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      </div>
    )
  }
  if (loadingLinks) {
    return (
      <div className="flex-1 overflow-auto">
        <div className="space-y-6">
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

  const linkedServices = (servicesLinked ?? []).map((link) => link.service)
  const availableServices = linkableDatabases(
    services ?? [],
    servicesLinked ?? [],
    search
  )
  // Linking is offered only once a database can be linked: none on the
  // server, or every one of a type the service already uses, and the
  // section is not shown.
  // One that could not be loaded is shown, so its retry is reachable.
  const linkableCount = linkableDatabases(
    services ?? [],
    servicesLinked ?? [],
    ''
  ).length
  const canLink = servicesError || linkableCount > 0

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-8">
        {header}

        <ServiceLinkRedeployPrompt
          project={project}
          change={linkChange}
          onDismiss={() => setLinkChange(null)}
        />

        <section aria-labelledby="linked-databases">
          <div className="mb-2 flex items-baseline justify-between">
            <h2
              id="linked-databases"
              className="text-sm font-medium text-foreground"
            >
              Linked
            </h2>
            <span className="text-xs text-muted-foreground tabular-nums">
              {linkedServices.length}
            </span>
          </div>
          {linkedServices.length > 0 ? (
            <ul
              role="list"
              className="divide-y divide-border rounded-lg border border-border overflow-hidden"
            >
              {linkedServices.map((service) => (
                <ServiceRow
                  key={service.id}
                  service={service}
                  isLinked
                  link={servicesLinked?.find(
                    (link) => link.service.id === service.id
                  )}
                  isBusy={isToggling}
                  projectSlug={project.slug}
                  onToggle={() => handleServiceToggle(service.id)}
                />
              ))}
            </ul>
          ) : (
            <p className="rounded-lg border border-dashed px-4 py-6 text-center text-sm text-muted-foreground">
              No databases are linked to this service yet.
            </p>
          )}
        </section>

        {canLink && (
          <section
            aria-labelledby="link-existing-database"
            className="space-y-3"
          >
            <h2
              id="link-existing-database"
              className="text-sm font-medium text-foreground"
            >
              Link an existing database
            </h2>
            {servicesError ? (
              <Alert variant="destructive">
                <AlertTitle>
                  The databases on this server could not be loaded
                </AlertTitle>
                <AlertDescription>
                  The databases linked above are unaffected.
                  <Button
                    variant="outline"
                    size="sm"
                    className="ml-3"
                    onClick={() => void refetchServices()}
                  >
                    Retry
                  </Button>
                </AlertDescription>
              </Alert>
            ) : (
              <>
                <p className="text-xs text-muted-foreground">
                  Linking gives this service its connection settings from the
                  next deployment.
                </p>
                {/* A few are read at a glance; more are found by name. */}
                {(linkableCount > LINKABLE_ROWS || search !== '') && (
                  <Input
                    aria-label="Search existing databases"
                    placeholder="Search by name or database type…"
                    className="max-w-md"
                    value={search}
                    onChange={(event) => setSearch(event.target.value)}
                  />
                )}
                {availableServices.length === 0 ? (
                  <p className="py-4 text-sm text-muted-foreground">
                    No databases match your search.
                  </p>
                ) : (
                  <>
                    <ul
                      role="list"
                      className="divide-y divide-border rounded-lg border border-border overflow-hidden"
                    >
                      {availableServices
                        .slice(0, LINKABLE_ROWS)
                        .map((service) => (
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
                    {availableServices.length > LINKABLE_ROWS && (
                      <p className="text-xs text-muted-foreground">
                        {`${availableServices.length - LINKABLE_ROWS} more. Search to find them.`}
                      </p>
                    )}
                  </>
                )}
              </>
            )}
          </section>
        )}
        {databaseTypes}
        <ConfirmDeleteDialog
          open={serviceToUnlink !== null}
          onOpenChange={(open) => {
            if (!open) setServiceToUnlink(null)
          }}
          verb="Unlink"
          resourceType="database service"
          resourceName={serviceToUnlink?.name ?? ''}
          consequences={`${project.slug} stops receiving its connection variables, and the running app loses access after its next deployment. The service itself and its data are kept.`}
          irreversible={false}
          onConfirm={() => {
            if (serviceToUnlink) void unlinkService(serviceToUnlink.id)
          }}
        />
        {servicePendingLink && (
          <DatabaseProvisioningDialog
            key={servicePendingLink.id}
            open
            serviceName={servicePendingLink.name}
            projectSlug={project.slug}
            isPending={linkServiceMutation.isPending}
            onOpenChange={(open) => {
              if (!open && !linkServiceMutation.isPending)
                setServicePendingLink(null)
            }}
            onConfirm={async (selection) => {
              try {
                await linkService(servicePendingLink.id, selection)
                setServicePendingLink(null)
              } catch {
                /* Keep the selection for retry; the mutation toast explains the failure. */
              }
            }}
          />
        )}
      </div>
    </div>
  )
}
