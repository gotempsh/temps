// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getEnvironmentsOptions,
  getProjectsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type {
  EnvironmentResponse,
  PluginServicesResponse,
  ProjectResponse,
  ServicePluginResponse,
} from '@/api/client/types.gen'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group'
import { Skeleton } from '@/components/ui/skeleton'
import { useDebounce } from '@/hooks/useDebounce'
import { useAddServicePlugin } from '@/hooks/usePlugins'
import {
  addServiceFailure,
  servicePluginPath,
  servicePluginSummary,
  servicePluginsNeedingAttention,
} from '@/lib/plugin-services'
import { zodResolver } from '@hookform/resolvers/zod'
import { Status } from '@temps-sdk/ds'
import { useQuery } from '@tanstack/react-query'
import { AlertCircle, Info, Loader2 } from 'lucide-react'
import { useEffect, useId } from 'react'
import { Controller, useForm, useWatch } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { z } from 'zod'

/** How many services the picker lists at once; search narrows the rest. */
const SERVICE_CHOICES = 8

/** Whether a service plugin is running, stopped while idle or unavailable. */
export function ServicePluginStatus({
  plugin,
}: {
  plugin: ServicePluginResponse
}) {
  const summary = servicePluginSummary(plugin)
  return (
    <span title={summary.detail}>
      <Status tone={summary.tone} label={summary.label} />
    </span>
  )
}

/** The service a plugin runs as, linked while that service exists. */
export function ServicePluginOrigin({
  plugin,
}: {
  plugin: ServicePluginResponse
}) {
  const path = servicePluginPath(plugin)
  if (!path || !plugin.service_name) {
    return (
      <p className="mt-1 text-xs text-muted-foreground">
        The service this plugin ran as was deleted.
      </p>
    )
  }
  return (
    <p className="mt-1 text-xs text-muted-foreground [overflow-wrap:anywhere]">
      Runs as the service{' '}
      <Link to={path} className="font-medium text-foreground underline">
        {plugin.service_name}
      </Link>
      {plugin.environment_name ? ` · ${plugin.environment_name}` : ''}
      {plugin.stops_when_idle ? ' · stops when idle' : ''}
    </p>
  )
}

/**
 * What an administrator has to know about plugins that run as services
 * before the list: that this server cannot run any (and why), and which ones
 * Temps is not connected to (and why), each removable from here because a
 * plugin Temps never reached has no row in the list below.
 */
export function ServicePluginNotice({
  services,
  unavailable,
  disabled,
  onRemove,
}: {
  services: PluginServicesResponse | undefined
  /** The status itself could not be read. */
  unavailable: boolean
  disabled?: boolean
  onRemove: (name: string) => void
}) {
  if (unavailable) {
    return (
      <Alert>
        <AlertCircle className="size-4" />
        <AlertTitle>Service plugins could not be read</AlertTitle>
        <AlertDescription>
          Temps could not read which plugins run as services. Reload the page to
          try again.
        </AlertDescription>
      </Alert>
    )
  }
  if (!services) return null
  const broken = servicePluginsNeedingAttention(services.plugins)
  if (broken.length === 0) return null
  return (
    <Alert variant="destructive">
      <AlertCircle className="size-4" />
      <AlertTitle>
        {broken.length === 1
          ? '1 plugin is not connected to its service'
          : `${broken.length} plugins are not connected to their services`}
      </AlertTitle>
      <AlertDescription>
        <ul className="space-y-2">
          {broken.map((plugin) => (
            <ServicePluginProblem
              key={plugin.name}
              plugin={plugin}
              disabled={disabled}
              onRemove={onRemove}
            />
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  )
}

function ServicePluginProblem({
  plugin,
  disabled,
  onRemove,
}: {
  plugin: ServicePluginResponse
  disabled?: boolean
  onRemove: (name: string) => void
}) {
  const path = servicePluginPath(plugin)
  return (
    <li className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
      <span className="min-w-0 [overflow-wrap:anywhere]">
        <strong>{plugin.name}:</strong> {servicePluginSummary(plugin).detail}
      </span>
      <span className="flex shrink-0 gap-2">
        {path && (
          <Button asChild variant="outline" size="sm">
            <Link to={path}>Open service</Link>
          </Button>
        )}
        <Button
          variant="outline"
          size="sm"
          disabled={disabled}
          onClick={() => onRemove(plugin.name)}
        >
          Remove plugin
        </Button>
      </span>
    </li>
  )
}

const schema = z.object({
  /** Narrows the list of services; not part of what is submitted. */
  search: z.string(),
  projectId: z
    .number()
    .int()
    .positive('Choose the service the plugin runs as.'),
  environmentId: z
    .number()
    .int()
    .positive('Choose the environment whose deployment is the plugin.'),
})

type Values = z.infer<typeof schema>

/**
 * Make a deployed service a plugin. Always reachable: on a server that cannot
 * run plugins as services it says what a plugin service is and what is
 * missing instead of hiding the action.
 */
export function AddServicePluginDialog({
  open,
  onOpenChange,
  services,
  onSensitiveError,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  services: PluginServicesResponse | undefined
  onSensitiveError: (error: unknown, retry: () => void) => boolean
}) {
  const configured = services?.configured !== false
  const add = useAddServicePlugin()
  const form = useForm<Values>({
    resolver: zodResolver(schema),
    defaultValues: { search: '', projectId: 0, environmentId: 0 },
  })
  const search = useWatch({ control: form.control, name: 'search' })
  const term = useDebounce(search.trim(), 250)
  const projects = useQuery({
    ...getProjectsOptions({
      query: {
        page: 1,
        per_page: SERVICE_CHOICES,
        ...(term === '' ? {} : { search: term }),
      },
    }),
    enabled: open && configured,
  })
  const projectId = useWatch({ control: form.control, name: 'projectId' })
  const environments = useQuery({
    ...getEnvironmentsOptions({ path: { project_id: projectId } }),
    enabled: open && configured && projectId > 0,
  })
  const { reset, setValue } = form
  const resetAdd = add.reset
  useEffect(() => {
    if (!open) return
    reset({ search: '', projectId: 0, environmentId: 0 })
    resetAdd()
  }, [open, reset, resetAdd])
  // A service with one environment needs no second choice.
  const onlyEnvironment =
    environments.data?.length === 1 ? environments.data[0].id : undefined
  useEffect(() => {
    if (onlyEnvironment !== undefined)
      setValue('environmentId', onlyEnvironment, { shouldValidate: true })
  }, [onlyEnvironment, setValue])

  async function submit(values: Values) {
    try {
      const added = await add.mutateAsync({
        project_id: values.projectId,
        environment_id: values.environmentId,
      })
      toast.success(
        `${added.display_name ?? added.name} is now a plugin. Grant it permissions to let it use Temps.`
      )
      onOpenChange(false)
    } catch (error) {
      // Any other failure is shown by the app-wide mutation error handler
      // with the server's reason, and repeated below the form.
      onSensitiveError(error, () => void submit(values))
    }
  }
  const errors = form.formState.errors
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!add.isPending) onOpenChange(next)
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add a plugin from a service</DialogTitle>
          <DialogDescription>
            A plugin is an ordinary service built with the Temps plugin SDK.
            Deploy it like any app, then choose it here. It starts with no
            permissions.
          </DialogDescription>
        </DialogHeader>
        {!configured && (
          <Alert>
            <Info className="size-4" />
            <AlertTitle>This server cannot run plugins as services</AlertTitle>
            <AlertDescription className="break-words">
              {services?.reason ??
                'This Temps process deploys no services, so it has none to run a plugin as.'}
            </AlertDescription>
          </Alert>
        )}
        <form className="space-y-4" onSubmit={form.handleSubmit(submit)}>
          <fieldset
            disabled={!configured || add.isPending}
            className="space-y-4"
          >
            <Controller
              control={form.control}
              name="projectId"
              render={({ field }) => (
                <ServiceChoice
                  search={search}
                  onSearch={(value) => setValue('search', value)}
                  loading={projects.isPending && configured}
                  failed={projects.isError}
                  services={projects.data?.projects ?? []}
                  total={projects.data?.total ?? 0}
                  value={field.value}
                  onChange={(id) => {
                    field.onChange(id)
                    setValue('environmentId', 0)
                  }}
                  error={errors.projectId?.message}
                />
              )}
            />
            <Controller
              control={form.control}
              name="environmentId"
              render={({ field }) => (
                <EnvironmentChoice
                  chosen={projectId > 0}
                  loading={environments.isPending}
                  failed={environments.isError}
                  environments={environments.data ?? []}
                  value={field.value}
                  onChange={field.onChange}
                  error={errors.environmentId?.message}
                />
              )}
            />
          </fieldset>
          {add.isError && (
            <p role="alert" className="text-sm text-destructive break-words">
              {addServiceFailure(add.error)}
            </p>
          )}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={add.isPending}
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!configured || add.isPending}>
              {add.isPending && (
                <Loader2 className="mr-2 size-4 animate-spin" />
              )}
              {add.isPending ? 'Connecting…' : 'Add plugin'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

export function ServiceChoice({
  search,
  onSearch,
  loading,
  failed,
  services,
  total,
  value,
  onChange,
  error,
}: {
  search: string
  onSearch: (value: string) => void
  loading: boolean
  failed: boolean
  services: ProjectResponse[]
  total: number
  value: number
  onChange: (id: number) => void
  error?: string
}) {
  const id = useId()
  return (
    <div className="space-y-2">
      <label htmlFor={`${id}-search`} className="text-sm font-medium">
        Service
      </label>
      <Input
        id={`${id}-search`}
        type="search"
        placeholder="Search services"
        value={search}
        onChange={(event) => onSearch(event.target.value)}
      />
      {loading ? (
        <div className="space-y-2" aria-hidden="true">
          <Skeleton className="h-11 w-full" />
          <Skeleton className="h-11 w-full" />
        </div>
      ) : failed ? (
        <p role="alert" className="text-sm text-destructive">
          Services could not be loaded. Close this dialog and try again.
        </p>
      ) : services.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {search.trim() === ''
            ? 'No services yet. Deploy the plugin as a service first.'
            : 'No service matches that search.'}
        </p>
      ) : (
        <RadioGroup
          aria-label="Service"
          value={value > 0 ? String(value) : ''}
          onValueChange={(next) => onChange(Number(next))}
          className="gap-0 divide-y overflow-hidden rounded-md border"
        >
          {services.map((service) => (
            <label
              key={service.id}
              htmlFor={`${id}-service-${service.id}`}
              className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/40"
            >
              <RadioGroupItem
                id={`${id}-service-${service.id}`}
                value={String(service.id)}
                className="shrink-0"
              />
              <ServiceIcon service={service} size="sm" />
              <span className="min-w-0 text-sm font-medium [overflow-wrap:anywhere]">
                {service.name}
              </span>
            </label>
          ))}
        </RadioGroup>
      )}
      {!loading && !failed && total > services.length && (
        <p className="text-xs text-muted-foreground">
          Showing {services.length} of {total}. Search to find another service.
        </p>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  )
}

export function EnvironmentChoice({
  chosen,
  loading,
  failed,
  environments,
  value,
  onChange,
  error,
}: {
  /** Whether a service is chosen; there is nothing to list before that. */
  chosen: boolean
  loading: boolean
  failed: boolean
  environments: EnvironmentResponse[]
  value: number
  onChange: (id: number) => void
  error?: string
}) {
  const id = useId()
  if (!chosen) return null
  return (
    <div className="space-y-2">
      <p id={`${id}-label`} className="text-sm font-medium">
        Environment
      </p>
      {loading ? (
        <Skeleton className="h-11 w-full" aria-hidden="true" />
      ) : failed ? (
        <p role="alert" className="text-sm text-destructive">
          The environments of this service could not be loaded.
        </p>
      ) : environments.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          This service has no environment yet. Deploy it first.
        </p>
      ) : (
        <RadioGroup
          aria-labelledby={`${id}-label`}
          value={value > 0 ? String(value) : ''}
          onValueChange={(next) => onChange(Number(next))}
          className="gap-0 divide-y overflow-hidden rounded-md border"
        >
          {environments.map((environment) => (
            <label
              key={environment.id}
              htmlFor={`${id}-environment-${environment.id}`}
              className="flex cursor-pointer items-center gap-3 px-3 py-2 hover:bg-muted/40"
            >
              <RadioGroupItem
                id={`${id}-environment-${environment.id}`}
                value={String(environment.id)}
                className="shrink-0"
              />
              <span className="min-w-0 text-sm font-medium [overflow-wrap:anywhere]">
                {environment.name}
              </span>
              {environment.sleeping && (
                <span className="text-xs text-muted-foreground">
                  stopped while idle
                </span>
              )}
            </label>
          ))}
        </RadioGroup>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
    </div>
  )
}
