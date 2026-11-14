// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Settings, Field, PageState } from '@temps-sdk/ds'
import { Workflow } from 'lucide-react'
import { Link, useParams } from 'react-router'
import { useForm, useWatch } from 'react-hook-form'
import { z } from 'zod'
import { zodResolver } from '@hookform/resolvers/zod'
import { toast } from 'sonner'
import type { WorkflowSettings as SettingsValue } from '@/api/client'
import {
  getWorkflowSettingsOptions,
  listProjectServicesOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { updateWorkflowSettings } from '@/api/client/sdk.gen'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { problemDetail } from '@/lib/api-problem'

const schema = z.object({
  max_retained_events: z.number().int().min(1).max(1000000),
  max_retained_event_bytes: z.number().int().min(1).max(1073741824),
  drain_timeout_secs: z.number().int().min(0).max(3600),
  host_service: z.string(),
  managed_store_service_id: z.string().regex(/^(default|[1-9]\d*)$/),
})
type Fields = z.infer<typeof schema>
export function WorkflowSettings() {
  const params = useParams()
  const path = {
    project_id: Number(params.projectId),
    environment_id: Number(params.environmentId),
  }
  const query = useQuery(getWorkflowSettingsOptions({ path }))
  if (query.isPending) return <Skeleton className="h-64 w-full" />
  if (query.isError)
    return (
      <PageState
        variant="failed"
        size="compact"
        icon={Workflow}
        title="Could not load workflow settings"
        description={problemDetail(query.error, 'Refresh to retry.')}
      />
    )
  return (
    <SettingsForm key={query.dataUpdatedAt} path={path} settings={query.data} />
  )
}
export function SettingsForm({
  path,
  settings,
}: {
  path: { project_id: number; environment_id: number }
  settings: SettingsValue
}) {
  const cache = useQueryClient()
  const services = useQuery(
    listProjectServicesOptions({ path: { project_id: path.project_id } })
  )
  const databases =
    services.data
      ?.map((link) => link.service)
      .filter((service) => service.service_type === 'postgres') ?? []
  const form = useForm<Fields>({
    resolver: zodResolver(schema),
    defaultValues: {
      max_retained_events: settings.max_retained_events,
      max_retained_event_bytes: settings.max_retained_event_bytes,
      drain_timeout_secs: settings.drain_timeout_secs,
      host_service: settings.host_service ?? '',
      managed_store_service_id:
        settings.managed_store_service_id?.toString() ?? 'default',
    },
  })
  const selectedStore = useWatch({
    control: form.control,
    name: 'managed_store_service_id',
  })
  const save = useMutation({
    mutationFn: async (value: Fields) => {
      await updateWorkflowSettings({
        path,
        body: {
          ...settings,
          ...value,
          host_service: value.host_service.trim() || null,
          managed_store_service_id:
            value.managed_store_service_id === 'default'
              ? null
              : Number(value.managed_store_service_id),
        },
        throwOnError: true,
      })
    },
    onSuccess: async () => {
      toast.success('Workflow settings saved')
      await cache.invalidateQueries({
        queryKey: getWorkflowSettingsOptions({ path }).queryKey,
      })
    },
    onError: (error) =>
      toast.error(problemDetail(error, 'Could not save workflow settings.')),
  })
  return (
    <Settings
      title="Workflow settings"
      saving={save.isPending}
      dirty={form.formState.isDirty}
      errors={{
        'Retained events': form.formState.errors.max_retained_events?.message,
        'Retained event payload bytes':
          form.formState.errors.max_retained_event_bytes?.message,
        'Drain timeout': form.formState.errors.drain_timeout_secs?.message,
      }}
      onSubmit={form.handleSubmit((value) => save.mutate(value))}
    >
      <Button asChild variant="outline">
        <Link to="/workflows">All workflows</Link>
      </Button>
      <Field
        label="Retained events"
        description="New events are rejected at this count until retention cleanup frees capacity. Existing events are preserved."
      >
        {(props) => (
          <Input
            {...props}
            type="number"
            min={1}
            max={1000000}
            {...form.register('max_retained_events', { valueAsNumber: true })}
          />
        )}
      </Field>
      <Field
        label="Retained event payload bytes"
        description="Total plaintext payload bytes retained by this environment. Database and index overhead is additional."
      >
        {(props) => (
          <Input
            {...props}
            type="number"
            min={1}
            max={1073741824}
            {...form.register('max_retained_event_bytes', {
              valueAsNumber: true,
            })}
          />
        )}
      </Field>
      <Field
        label="Journal storage"
        description="The control-plane database needs no setup. Managed PostgreSQL stores new runs’ journals separately; existing runs keep their original store. Scheduling stays in Temps. Keep the selected database linked and backed up."
      >
        {(props) => (
          <Select
            value={selectedStore}
            onValueChange={(value) =>
              form.setValue('managed_store_service_id', value, {
                shouldDirty: true,
                shouldValidate: true,
              })
            }
          >
            <SelectTrigger {...props}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="default">
                Control-plane PostgreSQL (default)
              </SelectItem>
              {databases.map((service) => (
                <SelectItem key={service.id} value={String(service.id)}>
                  {service.name}
                </SelectItem>
              ))}
              {settings.managed_store_service_id &&
                !databases.some(
                  (service) => service.id === settings.managed_store_service_id
                ) && (
                  <SelectItem value={String(settings.managed_store_service_id)}>
                    Current service #{settings.managed_store_service_id}
                  </SelectItem>
                )}
            </SelectContent>
          </Select>
        )}
      </Field>
      <p className="text-sm text-muted-foreground">
        {services.isPending
          ? 'Loading linked PostgreSQL services…'
          : services.isError
            ? 'Could not load linked services. Your current setting is preserved.'
            : databases.length === 0
              ? 'No managed PostgreSQL service is linked to this project.'
              : 'Choose a linked PostgreSQL service to opt in.'}{' '}
        <Link className="underline" to={`/projects/${path.project_id}/storage`}>
          Manage project storage
        </Link>
      </p>
      <Field
        label="Host service"
        optional
        description="Leave empty for the main service. For Compose apps, enter the service that hosts your workflow handler."
      >
        {(props) => <Input {...props} {...form.register('host_service')} />}
      </Field>
      <Field
        label="Deployment drain timeout (seconds)"
        error={form.formState.errors.drain_timeout_secs?.message}
        description="0 disables drain. With 1–3600 seconds, promotion pauses new invocations and waits for running attempts. On timeout, the old deployment remains active. Sleeping and waiting runs resume on the new code."
      >
        {(props) => (
          <Input
            {...props}
            type="number"
            min={0}
            max={3600}
            {...form.register('drain_timeout_secs', { valueAsNumber: true })}
          />
        )}
      </Field>
    </Settings>
  )
}
