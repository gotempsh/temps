// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  ExternalServiceInfo,
  ProjectResponse,
  ProjectServiceInfo,
} from '@/api/client'
import {
  externalServiceMetricsGetRangeOptions,
  linkServiceToProjectMutation,
  listProjectServicesOptions,
  listServicesOptions,
  getProvidersMetadataOptions,
  unlinkServiceFromProjectMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { MetricSparkline } from '@/components/charts/metric-sparkline'
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
import { ServiceLogo } from '@/components/ui/service-logo'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { cn, formatBytes } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ChevronRight,
  Database,
  Link2,
  Link2Off,
  MoreHorizontal,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { serviceCreateHref } from '@/lib/service-project-link'
import { projectServiceResourcePath } from '@/lib/database-provisioning'
import { toast } from 'sonner'

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
                    ? 'Per project'
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
  const [search, setSearch] = useState('')
  const [servicePendingLink, setServicePendingLink] =
    useState<ExternalServiceInfo | null>(null)
  const providers = useQuery({ ...getProvidersMetadataOptions(), retry: false })

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
    isError: servicesError,
  } = useQuery({
    ...listServicesOptions(),
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
    meta: { errorTitle: 'Failed to link service to project' },
    onSuccess: () => refetchServicesLinked(),
  })

  const unlinkServiceMutation = useMutation({
    ...unlinkServiceFromProjectMutation(),
    meta: { errorTitle: 'Failed to unlink service from project' },
    onSuccess: () => refetchServicesLinked(),
  })

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
      error: 'Failed to link service',
    })
    await promise
    await refetchServicesLinked()
  }

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
      const service = services?.find((item) => item.id === serviceId)
      if (
        service &&
        ['postgres', 'mariadb', 'mongodb'].includes(service.service_type)
      ) {
        setServicePendingLink(service)
        return
      }
      await linkService(serviceId).catch(() => {})
    }

    await refetchServicesLinked()
  }

  const linkedCount = servicesLinked?.length ?? 0
  const totalCount = services?.length ?? 0
  const isToggling =
    linkServiceMutation.isPending || unlinkServiceMutation.isPending

  const databaseTypes = (
    <section
      aria-label={
        services?.length
          ? 'Create a new database'
          : 'Create your first database'
      }
      className="space-y-4"
    >
      <div>
        <h2 className="text-lg font-semibold">
          {services?.length
            ? 'Create a new database'
            : 'Create your first database'}
        </h2>
        <p className="mt-1 text-sm text-muted-foreground">
          Choose a database type to configure a new database linked to{' '}
          {project.name}.
        </p>
      </div>
      {providers.isPending ? (
        <div
          role="status"
          aria-label="Loading database types"
          className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3"
        >
          {Array.from({ length: 5 }, (_, index) => (
            <div
              key={index}
              className="h-40 animate-pulse rounded-lg bg-muted"
            />
          ))}
        </div>
      ) : providers.isError ? (
        <Alert variant="destructive">
          <AlertTitle>Database types could not be loaded</AlertTitle>
          <AlertDescription>
            <Button
              variant="outline"
              size="sm"
              onClick={() => void providers.refetch()}
            >
              Retry database types
            </Button>
          </AlertDescription>
        </Alert>
      ) : providers.data?.length ? (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {providers.data.map((provider) => (
            <Link
              key={provider.service_type}
              to={serviceCreateHref(provider.service_type, project.id)}
              aria-label={`Create ${provider.display_name}`}
              className="group flex min-w-0 flex-col items-start rounded-lg border bg-background p-5 transition-colors hover:border-foreground/30 hover:bg-muted/40 focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
            >
              <div className="mb-4 flex size-10 items-center justify-center">
                <ServiceLogo service={provider.service_type} />
              </div>
              <h3 className="font-medium">{provider.display_name}</h3>
              <p className="mt-1 flex-1 text-sm text-muted-foreground">
                {provider.description}
              </p>
              <span className="mt-5 inline-flex items-center gap-2 text-sm font-medium">
                Create database{' '}
                <ChevronRight
                  aria-hidden="true"
                  className="size-4 transition-transform group-hover:translate-x-0.5"
                />
              </span>
            </Link>
          ))}
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">
          No database types are available.
        </p>
      )}
    </section>
  )

  const header = (
    <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
      <div>
        <h1 className="text-xl font-semibold tracking-tight sm:text-2xl">
          Databases
        </h1>
        <p className="mt-1 text-sm text-muted-foreground">
          Link an existing database or create a new one for this project.
          {totalCount > 0 ? (
            <span className="ml-1 tabular-nums">
              {linkedCount} of {totalCount} linked.
            </span>
          ) : null}
        </p>
      </div>
      <CreateServiceButton
        projectId={project.id}
        label="Create database"
        open={isCreateDropdownOpen}
        onOpenChange={setIsCreateDropdownOpen}
      />
    </div>
  )

  if (servicesError || linksError) {
    return (
      <div className="space-y-6 p-4 md:p-6">
        {header}
        <Alert variant="destructive">
          <AlertTitle>Databases could not be loaded</AlertTitle>
          <AlertDescription>
            {servicesError
              ? 'Could not load the available databases.'
              : 'Could not determine which databases are linked to this project.'}
            <Button
              variant="outline"
              size="sm"
              className="ml-3"
              onClick={() => {
                void refetchServices()
                void refetchServicesLinked()
              }}
            >
              Retry databases
            </Button>
          </AlertDescription>
        </Alert>
      </div>
    )
  }
  if (isLoadingServices || loadingLinks) {
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
          {databaseTypes}
        </div>
      </div>
    )
  }

  const linkedServices = services.filter((s) =>
    servicesLinked?.some((l) => l.service.id === s.id)
  )
  const availableServices = services.filter(
    (s) =>
      !servicesLinked?.some((l) => l.service.id === s.id) &&
      `${s.name} ${s.service_type}`.toLowerCase().includes(search.toLowerCase())
  )

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-8 p-4 md:p-6">
        {header}

        <p className="text-sm text-muted-foreground">
          Choose an existing database below, or create a new database of any
          supported type. Linking makes its connection settings available to
          this project.
        </p>

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
                  link={servicesLinked?.find(
                    (link) => link.service.id === service.id
                  )}
                  isBusy={isToggling}
                  projectSlug={project.slug}
                  onToggle={() => handleServiceToggle(service.id)}
                />
              ))}
            </ul>
          </section>
        ) : null}

        {services.length > linkedServices.length ? (
          <section>
            <div className="mb-2 flex items-baseline justify-between">
              <h2 className="text-sm font-medium text-foreground">
                Link an existing database
              </h2>
              <span className="text-xs text-muted-foreground tabular-nums">
                {availableServices.length}
              </span>
            </div>
            <Input
              aria-label="Search existing databases"
              placeholder="Search by name or database type…"
              className="mb-3 max-w-md"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            {!availableServices.length && (
              <p className="py-4 text-sm text-muted-foreground">
                No databases match your search.
              </p>
            )}
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
        ) : (
          <p className="text-sm text-muted-foreground">
            All existing databases are linked to this project. You can create
            another database above.
          </p>
        )}
        {databaseTypes}
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
