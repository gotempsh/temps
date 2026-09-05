// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Check,
  ChevronsUpDown,
  Database,
  Link2,
  Loader2,
  Network,
  Plus,
  Unlink,
} from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'
import type { ApplicationResponse, ExternalServiceInfo } from '@/api/client'
import {
  linkServiceToProjectMutation,
  listProjectServicesOptions,
  listServicesOptions,
  unlinkServiceFromProjectMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'
import { ServiceLogo } from '@/components/ui/service-logo'
import { cn } from '@/lib/utils'
import { useMutation, useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import {
  RichProjectPicker,
  type ProjectPickerItem,
  type ProjectPickerTone,
} from './RichProjectPicker'

type Props = {
  application: ApplicationResponse
}

export function ApplicationDataServicesPanel({ application }: Props) {
  const defaultProjectId =
    application.projects.find((project) => project.is_primary)?.id ??
    application.projects[0]?.id ??
    null
  const [requestedProjectId, setRequestedProjectId] = useState<number | null>(
    defaultProjectId
  )
  const [serviceId, setServiceId] = useState<number | null>(null)
  const projectId = application.projects.some(
    (project) => project.id === requestedProjectId
  )
    ? requestedProjectId
    : defaultProjectId
  const selectedProject = application.projects.find(
    (project) => project.id === projectId
  )
  const projectOptions = application.projects.map(applicationProjectOption)

  const servicesQuery = useQuery({
    ...listServicesOptions(),
    enabled: projectId !== null,
    staleTime: 30_000,
  })
  const linkedQuery = useQuery({
    ...listProjectServicesOptions({
      path: { project_id: projectId ?? 0 },
    }),
    enabled: projectId !== null,
    staleTime: 15_000,
  })
  const linkMutation = useMutation(linkServiceToProjectMutation())
  const unlinkMutation = useMutation(unlinkServiceFromProjectMutation())

  const linkedIds = new Set(
    (linkedQuery.data ?? []).map((link) => link.service.id)
  )
  const availableServices = (servicesQuery.data ?? []).filter(
    (service) => !linkedIds.has(service.id)
  )
  const busy = linkMutation.isPending || unlinkMutation.isPending

  if (projectId === null) {
    return (
      <section className="space-y-3 border-t border-border pt-5">
        <div className="space-y-1">
          <h2 className="text-base font-semibold tracking-tight">Databases</h2>
          <p className="text-sm text-muted-foreground">
            Databases are linked through an application project.
          </p>
        </div>
        <div className="flex items-start gap-2 rounded-lg border border-dashed border-border p-3">
          <Database className="size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0">
            <p className="text-sm font-medium">Add a project first</p>
            <p className="text-xs text-muted-foreground">
              Create a workspace project or link an existing one above, then its
              databases can join the private sandbox network.
            </p>
          </div>
        </div>
      </section>
    )
  }

  const linkService = async () => {
    if (projectId === null || serviceId === null) return
    try {
      await linkMutation.mutateAsync({
        path: { id: serviceId },
        body: { project_id: projectId },
      })
      await linkedQuery.refetch()
      setServiceId(null)
      toast.success('Database linked and sandbox network updated')
    } catch (cause) {
      toast.error(errorMessage(cause, 'Could not link the database.'))
    }
  }

  const unlinkService = async (id: number) => {
    if (projectId === null) return
    try {
      await unlinkMutation.mutateAsync({
        path: { id, project_id: projectId },
      })
      await linkedQuery.refetch()
      toast.success('Database unlinked from the project and sandbox')
    } catch (cause) {
      toast.error(errorMessage(cause, 'Could not unlink the database.'))
    }
  }

  return (
    <section className="space-y-4 border-t border-border pt-5">
      <div className="space-y-1">
        <div className="flex items-center justify-between gap-3">
          <h2 className="text-base font-semibold tracking-tight">Databases</h2>
          {(linkedQuery.data?.length ?? 0) > 0 && (
            <span className="text-xs tabular-nums text-muted-foreground">
              {linkedQuery.data?.length} linked
            </span>
          )}
        </div>
        <p className="text-sm text-muted-foreground">
          Linked data services provide project runtime variables and join this
          workspace&apos;s private sandbox network.
        </p>
      </div>

      {application.projects.length > 1 && (
        <div className="space-y-2">
          <p className="text-xs font-medium text-muted-foreground">
            Manage databases for
          </p>
          <RichProjectPicker
            ariaLabel="Project whose databases to manage"
            disabled={busy}
            onValueChange={(nextProjectId) => {
              setRequestedProjectId(nextProjectId)
              setServiceId(null)
            }}
            projects={projectOptions}
            value={projectId}
          />
        </div>
      )}

      {servicesQuery.isPending || linkedQuery.isPending ? (
        <div className="space-y-2" aria-label="Loading databases">
          {Array.from({ length: 2 }).map((_, index) => (
            <div
              className="h-14 animate-pulse rounded-lg bg-muted"
              key={index}
            />
          ))}
        </div>
      ) : servicesQuery.isError || linkedQuery.isError ? (
        <div className="rounded-lg border border-destructive/30 bg-destructive/5 p-3">
          <p className="text-sm text-destructive">
            Could not load databases for this project.
          </p>
          <Button
            className="mt-2"
            onClick={() => {
              void servicesQuery.refetch()
              void linkedQuery.refetch()
            }}
            size="sm"
            type="button"
            variant="outline"
          >
            Try again
          </Button>
        </div>
      ) : (
        <>
          {(linkedQuery.data?.length ?? 0) > 0 ? (
            <ul
              className="divide-y divide-border overflow-hidden rounded-lg border border-border"
              role="list"
            >
              {linkedQuery.data?.map(({ service }) => (
                <li
                  className="flex min-w-0 items-center gap-3 p-3"
                  key={service.id}
                >
                  <ServiceLogo
                    className="size-7"
                    service={service.service_type}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex min-w-0 items-center gap-2">
                      <Link
                        className="truncate text-sm font-medium hover:underline"
                        to={`/storage/${service.id}`}
                      >
                        {service.name}
                      </Link>
                      <ServiceStatus service={service} />
                    </div>
                    <p className="truncate text-xs text-muted-foreground">
                      {serviceTypeLabel(service.service_type)}
                      {service.version ? ` ${service.version}` : ''} · private
                      network
                    </p>
                  </div>
                  <Button
                    aria-label={`Unlink ${service.name}`}
                    className="relative shrink-0"
                    disabled={busy}
                    onClick={() => void unlinkService(service.id)}
                    size="icon"
                    title={`Unlink ${service.name}`}
                    type="button"
                    variant="ghost"
                  >
                    {unlinkMutation.isPending &&
                    unlinkMutation.variables?.path.id === service.id ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Unlink className="size-4" />
                    )}
                    <span
                      aria-hidden="true"
                      className="absolute top-1/2 left-1/2 size-[max(100%,3rem)] -translate-1/2 pointer-fine:hidden"
                    />
                  </Button>
                </li>
              ))}
            </ul>
          ) : (
            <div className="rounded-lg border border-dashed border-border p-3">
              <div className="flex items-start gap-2">
                <Database className="size-4 shrink-0 text-muted-foreground" />
                <div className="min-w-0">
                  <p className="text-sm font-medium">No databases linked</p>
                  <p className="text-xs text-muted-foreground">
                    Link one below to make it available to{' '}
                    {selectedProject?.name ?? 'this project'}.
                  </p>
                </div>
              </div>
            </div>
          )}

          <div className="space-y-2 border-t border-border pt-4">
            <p className="text-xs font-medium text-muted-foreground">
              Add a data service
            </p>
            {availableServices.length > 0 && (
              <div className="flex min-w-0 gap-2">
                <RichServicePicker
                  disabled={busy}
                  onValueChange={setServiceId}
                  services={availableServices}
                  value={serviceId}
                />
                <Button
                  className="shrink-0"
                  disabled={busy || serviceId === null || projectId === null}
                  onClick={() => void linkService()}
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  {linkMutation.isPending ? (
                    <Loader2 className="size-4 animate-spin" />
                  ) : (
                    <Link2 className="size-4" />
                  )}
                  Link
                </Button>
              </div>
            )}
            <Button asChild className="w-full" size="sm" variant="outline">
              <Link
                to={`/storage/create${projectId === null ? '' : `?project_id=${projectId}`}`}
              >
                <Plus className="size-4" />
                Create new database
              </Link>
            </Button>
          </div>
        </>
      )}

      <div className="flex items-start gap-2 border-t border-border pt-3">
        <Network className="size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
        <p className="text-xs text-muted-foreground">
          Temps updates network access immediately. The sandbox receives no
          reusable platform token, and unrelated databases stay isolated.
        </p>
      </div>
    </section>
  )
}

function RichServicePicker({
  services,
  value,
  onValueChange,
  disabled,
}: {
  services: ExternalServiceInfo[]
  value: number | null
  onValueChange: (serviceId: number) => void
  disabled: boolean
}) {
  const [open, setOpen] = useState(false)
  const selected = services.find((service) => service.id === value) ?? null

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          aria-expanded={open}
          aria-label="Database to link"
          className="h-9 min-w-0 flex-1 justify-between gap-2 px-2 font-normal"
          disabled={disabled}
          role="combobox"
          type="button"
          variant="outline"
        >
          {selected ? (
            <span className="flex min-w-0 items-center gap-2">
              <ServiceLogo className="size-5" service={selected.service_type} />
              <span className="truncate text-sm">{selected.name}</span>
            </span>
          ) : (
            <span className="truncate text-sm text-muted-foreground">
              Choose a database…
            </span>
          )}
          <ChevronsUpDown className="size-4 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="w-[min(calc(100vw-2rem),22rem)] min-w-[var(--radix-popover-trigger-width)] p-0"
      >
        <Command>
          <CommandInput name="database-filter" placeholder="Find a database…" />
          <CommandList className="max-h-80">
            <CommandEmpty>No databases found.</CommandEmpty>
            <CommandGroup heading="Available databases">
              {services.map((service) => (
                <CommandItem
                  className="items-center gap-2 p-2"
                  key={service.id}
                  onSelect={() => {
                    onValueChange(service.id)
                    setOpen(false)
                  }}
                  value={`${service.name} ${service.service_type} ${service.status}`}
                >
                  <ServiceLogo
                    className="size-6"
                    service={service.service_type}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="flex min-w-0 items-center gap-2">
                      <span className="truncate text-sm font-medium">
                        {service.name}
                      </span>
                      <ServiceStatus service={service} />
                    </span>
                    <span className="text-xs text-muted-foreground">
                      {serviceTypeLabel(service.service_type)}
                      {service.version ? ` ${service.version}` : ''}
                    </span>
                  </span>
                  <Check
                    className={cn(
                      'size-4 shrink-0',
                      value === service.id ? 'opacity-100' : 'opacity-0'
                    )}
                  />
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}

function ServiceStatus({ service }: { service: ExternalServiceInfo }) {
  return (
    <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
      <span
        aria-hidden="true"
        className={cn(
          'size-2 rounded-full',
          service.status === 'running'
            ? 'bg-emerald-500'
            : service.status === 'failed'
              ? 'bg-red-500'
              : service.status === 'creating' || service.status === 'starting'
                ? 'bg-amber-500'
                : 'bg-zinc-400'
        )}
      />
      <span className="capitalize">{service.status}</span>
    </span>
  )
}

function applicationProjectOption(
  project: ApplicationResponse['projects'][number]
): ProjectPickerItem {
  const status = project.environments.find(
    (environment) => environment.slug === 'production'
  )?.deployment_state
  return {
    id: project.id,
    name: project.name,
    slug: project.slug,
    status: status ? status.replace(/_/g, ' ') : 'Not deployed',
    tone: deploymentTone(status),
  }
}

function deploymentTone(status: string | null | undefined): ProjectPickerTone {
  if (status === 'ready' || status === 'healthy' || status === 'running') {
    return 'healthy'
  }
  if (status === 'failed' || status === 'down' || status === 'error') {
    return 'down'
  }
  if (status === 'building' || status === 'deploying' || status === 'pending') {
    return 'warning'
  }
  return 'neutral'
}

function serviceTypeLabel(serviceType: string): string {
  switch (serviceType) {
    case 'postgres':
      return 'PostgreSQL'
    case 'mongodb':
      return 'MongoDB'
    case 'mariadb':
      return 'MariaDB'
    case 'redis':
    case 'kv':
      return 'Redis'
    case 's3':
    case 'blob':
    case 'rustfs':
      return 'Object storage'
    default:
      return serviceType
  }
}

function errorMessage(cause: unknown, fallback: string): string {
  return cause instanceof Error && cause.message.trim()
    ? cause.message
    : fallback
}
