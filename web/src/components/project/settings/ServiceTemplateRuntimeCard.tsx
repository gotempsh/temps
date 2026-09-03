// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ProjectResponse } from '@/api/client'
import {
  getProjectTemplateOptions,
  updateServiceTemplateRuntimeMutation,
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
import { Textarea } from '@/components/ui/textarea'
import {
  serviceTemplateRuntimeDefaults,
  templateRuntimeDefaults,
  templateRuntimeDefaultsSchema,
  templateRuntimeOverrides,
  type TemplateRuntimeDefaults,
} from '@/lib/template-runtime-defaults'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Boxes, RefreshCw, RotateCcw, Save } from 'lucide-react'
import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { toast } from 'sonner'

export function ServiceTemplateRuntimeCard({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  const templateSlug = project.template_slug ?? ''
  const templateQuery = useQuery({
    ...getProjectTemplateOptions({ path: { slug: templateSlug } }),
    enabled: templateSlug.length > 0,
    retry: false,
  })
  const updateRuntime = useMutation({
    ...updateServiceTemplateRuntimeMutation(),
    meta: { errorTitle: 'Failed to update service runtime' },
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
      serviceTemplateRuntimeDefaults(project, templateQuery.data ?? {})
    )
  }, [form, project, templateQuery.data])

  const template = templateQuery.data
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
        error: 'Failed to save service runtime',
      }
    )
  }

  return (
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
                disabled={templateQuery.isFetching}
                onClick={() => templateQuery.refetch()}
              >
                <RefreshCw
                  className={`mr-1.5 size-3.5 ${templateQuery.isFetching ? 'animate-spin' : ''}`}
                />
                Retry template details
              </Button>
            )}
          </CardHeader>

          <CardContent className="space-y-6">
            {!template && !templateQuery.isPending && (
              <div className="rounded-lg border border-amber-500/30 bg-amber-500/5 px-3 py-2 text-sm text-muted-foreground">
                The catalog definition is currently unavailable. You can still
                edit and save this project&apos;s stored runtime; only resetting
                to catalog defaults is unavailable.
              </div>
            )}
            <div className="flex items-start gap-3 rounded-lg border bg-muted/25 p-3 text-sm">
              <Boxes className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
              <p className="text-muted-foreground">
                Temps pulls this image directly; there is no source build. A new
                image version is applied when you deploy again. Existing
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
  )
}
