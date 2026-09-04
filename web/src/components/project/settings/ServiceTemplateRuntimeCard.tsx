// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  EnvVarTemplate,
  ProjectResponse,
  ServiceTemplateInstanceResponse,
} from '@/api/client'
import {
  getProjectServiceTemplateOptions,
  updateServiceTemplateRuntimeMutation,
  upgradeProjectServiceTemplateMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { TemplateImage } from '@/components/templates/TemplateImage'
import { Button } from '@/components/ui/button'
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from '@/components/ui/card'
import {
  Form,
  FormControl,
  FormDescription,
  FormField,
  FormItem,
  FormLabel,
  FormMessage,
} from '@/components/ui/form'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { templateEnvironmentVariableDefaultsToSecret } from '@/lib/project-environment-variables'
import { legacyDatabasesRedirectPath } from '@/lib/project-detail-routes'
import {
  serviceTemplateRuntimeDefaults,
  templateRuntimeDefaults,
  templateRuntimeDefaultsSchema,
  templateRuntimeOverrides,
  type TemplateRuntimeDefaults,
} from '@/lib/template-runtime-defaults'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  ArrowUpCircle,
  Boxes,
  Check,
  Database,
  RefreshCw,
  RotateCcw,
  Save,
} from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { useForm } from 'react-hook-form'
import { Link } from 'react-router'
import { toast } from 'sonner'
import { getErrorMessage } from '@/utils/errorHandling'

export function ServiceTemplateRuntimeCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const instanceQuery = useQuery({
    ...getProjectServiceTemplateOptions({
      path: { project_id: project.id },
    }),
    retry: false,
  })
  const updateRuntime = useMutation({
    ...updateServiceTemplateRuntimeMutation(),
  })
  const form = useForm<TemplateRuntimeDefaults>({
    resolver: zodResolver(templateRuntimeDefaultsSchema),
    defaultValues: {
      image: '',
      command: '',
      cpuRequest: '',
      cpuLimit: '',
      memoryRequest: '',
      memoryLimit: '',
      exposedPort: '',
      healthCheckPath: '/',
    },
  })

  useEffect(() => {
    form.reset(
      serviceTemplateRuntimeDefaults(
        project,
        instanceQuery.data?.applied.template ?? {}
      )
    )
  }, [form, project, instanceQuery.data?.applied.template])

  const template = instanceQuery.data?.applied.template
  const appliedVersion = instanceQuery.data?.applied.version
  const templateSlug =
    instanceQuery.data?.applied.slug ?? project.template_slug ?? ''
  const templateName =
    template?.name ??
    (templateSlug
      .split('-')
      .filter(Boolean)
      .map((part) => part[0]?.toUpperCase() + part.slice(1))
      .join(' ') ||
      'Service')
  const isPending = updateRuntime.isPending

  const save = async (values: TemplateRuntimeDefaults) => {
    const runtime = templateRuntimeOverrides(values)
    await toast.promise(
      (async () => {
        await updateRuntime.mutateAsync({
          path: { project_id: project.id },
          body: {
            imageRef: runtime.image,
            command: runtime.command,
            healthCheckPath: runtime.health_check_path,
            cpuRequest: runtime.cpu_request ?? null,
            cpuLimit: runtime.cpu_limit ?? null,
            memoryRequest: runtime.memory_request ?? null,
            memoryLimit: runtime.memory_limit ?? null,
            exposedPort: runtime.exposed_port ?? null,
          },
        })
        await refetch()
      })(),
      {
        loading: 'Saving service runtime…',
        success: 'Service runtime saved for the next deployment',
        error: (error) =>
          getErrorMessage(error, 'Failed to save service runtime'),
      }
    )
  }

  return (
    <div className="space-y-6">
      {instanceQuery.isPending ? (
        <Card>
          <CardHeader className="space-y-3">
            <Skeleton className="h-6 w-64" />
            <Skeleton className="h-4 w-full max-w-xl" />
          </CardHeader>
          <CardContent className="space-y-3">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-24 w-full" />
          </CardContent>
        </Card>
      ) : instanceQuery.data ? (
        <ServiceTemplateUpgradeCard
          key={`${instanceQuery.data.applied.version}-${instanceQuery.data.latest?.version ?? 'none'}`}
          project={project}
          instance={instanceQuery.data}
          onUpgraded={async () => {
            await instanceQuery.refetch()
            await refetch()
          }}
        />
      ) : (
        <Card className="border-amber-500/30">
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <AlertTriangle className="size-4 text-amber-500" />
              Service template details unavailable
            </CardTitle>
            <CardDescription>
              Temps could not read this project&apos;s saved template release.
              Its current runtime is still editable, but upgrades and resetting
              to template defaults are disabled until this is resolved.
            </CardDescription>
          </CardHeader>
          <CardFooter>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={instanceQuery.isFetching}
              onClick={() => instanceQuery.refetch()}
            >
              <RefreshCw
                className={`mr-1.5 size-3.5 ${instanceQuery.isFetching ? 'animate-spin' : ''}`}
              />
              Retry
            </Button>
          </CardFooter>
        </Card>
      )}

      <Form {...form}>
        <form onSubmit={form.handleSubmit(save)}>
          <Card>
            <CardHeader className="gap-4 sm:flex-row sm:items-start sm:justify-between">
              <div className="flex min-w-0 gap-3">
                {template ? (
                  <TemplateImage
                    imageUrl={template.image_url}
                    preset={template.preset}
                    alt={template.name}
                    className="size-11 shrink-0 border"
                    imgClassName="size-9"
                  />
                ) : (
                  <div className="flex size-11 shrink-0 items-center justify-center rounded-md border bg-muted/40">
                    <Boxes className="size-5 text-muted-foreground" />
                  </div>
                )}
                <div className="min-w-0 space-y-1">
                  <CardTitle className="flex items-center gap-2">
                    {templateName} runtime
                    <span className="rounded-full border px-2 py-0.5 text-[11px] font-medium text-muted-foreground">
                      Service template
                    </span>
                    {appliedVersion && (
                      <span className="rounded-full border px-2 py-0.5 font-mono text-[11px] font-medium text-muted-foreground">
                        v{appliedVersion}
                      </span>
                    )}
                  </CardTitle>
                  <CardDescription>
                    This project was created from the {templateName} template.
                    Changes below become the source of truth for future deploys.
                  </CardDescription>
                </div>
              </div>
              {template ? (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  disabled={isPending}
                  onClick={() =>
                    form.reset(templateRuntimeDefaults(template), {
                      keepDefaultValues: true,
                    })
                  }
                >
                  <RotateCcw className="mr-1.5 size-3.5" />
                  Reset template defaults
                </Button>
              ) : (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  disabled={instanceQuery.isFetching}
                  onClick={() => instanceQuery.refetch()}
                >
                  <RefreshCw
                    className={`mr-1.5 size-3.5 ${instanceQuery.isFetching ? 'animate-spin' : ''}`}
                  />
                  Retry saved template
                </Button>
              )}
            </CardHeader>

            <CardContent className="space-y-6">
              {!template && !instanceQuery.isPending && (
                <div className="rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-sm text-muted-foreground">
                  The saved template definition is currently unavailable. You
                  can still edit and save this project&apos;s stored runtime;
                  only resetting to template defaults is unavailable.
                </div>
              )}
              <div className="flex items-start gap-3 rounded-lg border bg-muted/25 p-3 text-sm">
                <Boxes className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <p className="text-muted-foreground">
                  Temps pulls this image directly; there is no source build. A
                  new image version is applied when you deploy again. Existing
                  containers keep running until that deployment is ready.
                </p>
              </div>

              <div className="grid gap-5 md:grid-cols-2">
                <FormField
                  control={form.control}
                  name="image"
                  render={({ field }) => (
                    <FormItem className="md:col-span-2">
                      <FormLabel htmlFor="service-runtime-image">
                        Container image
                      </FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          id="service-runtime-image"
                          className="font-mono text-sm"
                          autoCapitalize="none"
                          autoCorrect="off"
                          spellCheck={false}
                        />
                      </FormControl>
                      <FormDescription>
                        Pin a version or digest for repeatable deployments.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                <FormField
                  control={form.control}
                  name="command"
                  render={({ field }) => (
                    <FormItem className="md:col-span-2">
                      <FormLabel htmlFor="service-runtime-command">
                        Container command
                      </FormLabel>
                      <FormControl>
                        <Textarea
                          {...field}
                          id="service-runtime-command"
                          rows={2}
                          className="resize-y font-mono text-sm"
                          placeholder={'start\n--optimized'}
                          autoCapitalize="none"
                          autoCorrect="off"
                          spellCheck={false}
                        />
                      </FormControl>
                      <FormDescription>
                        One argument per line. Leave empty to use the image
                        default. Values are passed as argv without a shell.
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />

                {(
                  [
                    [
                      'cpuRequest',
                      'CPU request',
                      'Guaranteed CPU cores',
                      '0.5',
                      '0.01',
                      '0.01',
                    ],
                    [
                      'cpuLimit',
                      'CPU limit',
                      'Maximum cores; 0 is uncapped',
                      '1',
                      '0',
                      '0.01',
                    ],
                    [
                      'memoryRequest',
                      'Memory request',
                      'Guaranteed memory in MiB',
                      '512',
                      '1',
                      '1',
                    ],
                    [
                      'memoryLimit',
                      'Memory limit',
                      'Maximum MiB; 0 is uncapped',
                      '1536',
                      '0',
                      '1',
                    ],
                  ] as const
                ).map(([name, label, description, placeholder, min, step]) => (
                  <FormField
                    key={name}
                    control={form.control}
                    name={name}
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel htmlFor={`service-runtime-${name}`}>
                          {label}
                        </FormLabel>
                        <FormControl>
                          <Input
                            {...field}
                            id={`service-runtime-${name}`}
                            type="number"
                            min={min}
                            step={step}
                            placeholder={placeholder}
                          />
                        </FormControl>
                        <FormDescription>{description}</FormDescription>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                ))}

                <FormField
                  control={form.control}
                  name="exposedPort"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel htmlFor="service-runtime-port">
                        Container port
                      </FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          id="service-runtime-port"
                          type="number"
                          min="1"
                          max="65535"
                        />
                      </FormControl>
                      <FormDescription>Receives public traffic</FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
                <FormField
                  control={form.control}
                  name="healthCheckPath"
                  render={({ field }) => (
                    <FormItem>
                      <FormLabel htmlFor="service-runtime-health-path">
                        Health-check path
                      </FormLabel>
                      <FormControl>
                        <Input
                          {...field}
                          id="service-runtime-health-path"
                          className="font-mono text-sm"
                        />
                      </FormControl>
                      <FormDescription>
                        Relative HTTP path used for readiness
                      </FormDescription>
                      <FormMessage />
                    </FormItem>
                  )}
                />
              </div>
            </CardContent>
            <CardFooter className="justify-end border-t pt-6">
              <Button
                type="submit"
                disabled={isPending || !form.formState.isDirty}
              >
                <Save className="mr-2 size-4" />
                Save runtime
              </Button>
            </CardFooter>
          </Card>
        </form>
      </Form>
    </div>
  )
}

function ServiceTemplateUpgradeCard({
  project,
  instance,
  onUpgraded,
}: {
  project: ProjectResponse
  instance: ServiceTemplateInstanceResponse
  onUpgraded: () => Promise<void>
}) {
  const upgrade = useMutation({
    ...upgradeProjectServiceTemplateMutation(),
  })
  const latest = instance.latest
  const requiredConfiguration = instance.required_configuration
  const [configuration, setConfiguration] = useState<Record<string, string>>(
    () =>
      Object.fromEntries(
        requiredConfiguration.map((variable) => [
          variable.name,
          variable.default ?? '',
        ])
      )
  )
  const missingValues = useMemo(
    () =>
      requiredConfiguration.filter(
        (variable) => !(configuration[variable.name] ?? '').trim()
      ),
    [configuration, requiredConfiguration]
  )

  if (!latest) {
    return instance.catalog_error ? (
      <ServiceTemplateCatalogUnavailable message={instance.catalog_error} />
    ) : null
  }

  if (!instance.upgrade_available && !instance.catalog_drift) {
    return null
  }

  const blocked =
    instance.catalog_drift ||
    instance.missing_services.length > 0 ||
    missingValues.length > 0

  const applyUpgrade = async () => {
    if (!instance.upgrade_available || blocked) return

    await toast.promise(
      (async () => {
        await upgrade.mutateAsync({
          path: { project_id: project.id },
          body: {
            target_version: latest.version,
            environment_variables: requiredConfiguration.map((variable) => ({
              name: variable.name,
              value: configuration[variable.name] ?? '',
              is_secret: isSecretTemplateVariable(
                variable,
                latest.template.kind
              ),
            })),
          },
        })
        await onUpgraded()
      })(),
      {
        loading: `Applying ${latest.slug} ${latest.version}…`,
        success: 'Template update saved for the next deployment',
        error: (error) =>
          getErrorMessage(error, 'Failed to update service template'),
      }
    )
  }

  return (
    <Card className="border-primary/25">
      <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="space-y-1">
          <CardTitle className="flex items-center gap-2 text-base">
            <ArrowUpCircle className="size-4 text-primary" />
            Template update
          </CardTitle>
          <CardDescription>
            {instance.applied.slug}{' '}
            <span className="font-mono">{instance.applied.version}</span>
            {' → '}
            <span className="font-mono">{latest.version}</span>. Review and save
            it, then deploy when you are ready.
          </CardDescription>
        </div>
        <span className="w-fit rounded-full border border-primary/25 bg-primary/5 px-2.5 py-1 text-xs font-medium text-primary">
          Update available
        </span>
      </CardHeader>
      <CardContent className="space-y-5">
        {instance.catalog_drift && (
          <div className="flex gap-3 rounded-lg border border-destructive/30 bg-destructive/5 p-3 text-sm">
            <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
            <div>
              <p className="font-medium">This release cannot be applied</p>
              <p className="mt-1 text-muted-foreground">
                The catalog changed without a version bump. Publish a new
                template version so upgrades remain reproducible.
              </p>
            </div>
          </div>
        )}

        {instance.missing_services.length > 0 && (
          <div className="flex gap-3 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm">
            <Database className="mt-0.5 size-4 shrink-0 text-amber-600" />
            <div className="flex-1">
              <p className="font-medium">Link the required managed service</p>
              <p className="mt-1 text-muted-foreground">
                Add {instance.missing_services.join(', ')} to this project
                before applying the update. Existing service links are never
                removed automatically.
              </p>
              <Button variant="outline" size="sm" className="mt-3" asChild>
                <Link to={legacyDatabasesRedirectPath(project.slug)}>
                  <Database className="mr-2 size-4" />
                  Manage databases
                </Link>
              </Button>
            </div>
          </div>
        )}

        {instance.changes.length > 0 && (
          <div className="rounded-lg border">
            <div className="border-b px-3 py-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
              What changes
            </div>
            <div className="divide-y">
              {instance.changes.map((change, index) => (
                <div
                  key={`${change.field}-${index}`}
                  className="grid gap-1 px-3 py-2.5 text-sm sm:grid-cols-[11rem_1fr]"
                >
                  <span className="font-medium">{change.field}</span>
                  <span className="min-w-0 break-all font-mono text-xs text-muted-foreground">
                    {formatTemplateChange(change.current)} →{' '}
                    {formatTemplateChange(change.target)}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}

        {requiredConfiguration.length > 0 && (
          <div className="space-y-3">
            <div>
              <p className="text-sm font-medium">New required configuration</p>
              <p className="text-sm text-muted-foreground">
                These values are added to production. Existing environment
                variables are preserved and cannot be overwritten here.
              </p>
            </div>
            <div className="grid gap-4 md:grid-cols-2">
              {requiredConfiguration.map((variable) => {
                const secret = isSecretTemplateVariable(
                  variable,
                  latest.template.kind
                )
                return (
                  <label key={variable.name} className="space-y-1.5">
                    <span className="text-sm font-medium">
                      {variable.name}{' '}
                      <span className="text-destructive">*</span>
                    </span>
                    <Input
                      type={secret ? 'password' : 'text'}
                      value={configuration[variable.name] ?? ''}
                      placeholder={variable.example ?? undefined}
                      autoCapitalize="none"
                      autoCorrect="off"
                      spellCheck={false}
                      onChange={(event) =>
                        setConfiguration((current) => ({
                          ...current,
                          [variable.name]: event.target.value,
                        }))
                      }
                    />
                    {variable.description && (
                      <span className="block text-xs text-muted-foreground">
                        {variable.description}
                      </span>
                    )}
                  </label>
                )
              })}
            </div>
          </div>
        )}
      </CardContent>
      <CardFooter className="justify-between gap-4 border-t pt-6">
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Check className="size-3.5" />
          Custom runtime values are preserved when defaults change.
        </p>
        <Button
          type="button"
          disabled={blocked || upgrade.isPending || !instance.upgrade_available}
          onClick={applyUpgrade}
        >
          <ArrowUpCircle className="mr-2 size-4" />
          Apply template update
        </Button>
      </CardFooter>
    </Card>
  )
}

export function ServiceTemplateCatalogUnavailable({
  message,
}: {
  message: string
}) {
  return (
    <Card className="border-amber-500/30">
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <AlertTriangle className="size-4 text-amber-500" />
          Template updates unavailable
        </CardTitle>
        <CardDescription>
          {message} Your saved runtime remains deployable and editable below.
        </CardDescription>
      </CardHeader>
    </Card>
  )
}

function isSecretTemplateVariable(
  variable: EnvVarTemplate,
  templateKind: string | undefined
): boolean {
  return templateEnvironmentVariableDefaultsToSecret({
    templateKind,
    key: variable.name,
    defaultGenerator: variable.default_generator,
    explicitSecret: variable.secret,
  })
}

function formatTemplateChange(value: string | null | undefined): string {
  const normalized = value?.trim()
  return normalized ? normalized : 'not set'
}
