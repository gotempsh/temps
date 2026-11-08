// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useForm } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { useMutation } from '@tanstack/react-query'
import { Callout, Field, Settings } from '@temps-sdk/ds'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  Input,
} from '@temps-sdk/ui'
import { toast } from 'sonner'
import { requestServiceGrant, type AccessServiceResponse } from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import { grantRequestSchema } from './service-access-form'

export function ServiceGrantEditor({
  projectId,
  sandboxId,
  service,
  onClose,
  onSaved,
}: {
  projectId?: number | null
  sandboxId: string
  service?: AccessServiceResponse
  onClose: () => void
  onSaved: () => void
}) {
  const form = useForm<{ ttl: number }>({
    resolver: zodResolver(grantRequestSchema),
    defaultValues: { ttl: 900 },
  })
  const save = useMutation({
    mutationFn: async ({ ttl }: { ttl: number }) => {
      if (!projectId || !service)
        throw new Error('Choose a service before requesting access')
      if (ttl > service.max_ttl_seconds)
        throw new Error('Lifetime exceeds this service’s maximum')
      return requestServiceGrant({
        path: { project_id: projectId, sandbox_id: sandboxId },
        body: {
          service_id: service.id,
          service_version: service.version,
          ttl_seconds: ttl,
        },
        throwOnError: true,
      })
    },
    onSuccess: () => {
      toast.success('Access request saved', {
        description: 'The sandbox is not connected. The lifetime starts now.',
      })
      onSaved()
      onClose()
    },
  })
  const resetMutation = save.reset
  useEffect(() => {
    form.reset({ ttl: Math.min(900, service?.max_ttl_seconds ?? 900) })
    resetMutation()
  }, [service, form, resetMutation])
  return (
    <Dialog
      open={!!service}
      onOpenChange={(open) => {
        if (!open && !save.isPending) onClose()
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Request access to {service?.name}</DialogTitle>
          <DialogDescription>
            The Tokenless integration is not installed. This saves a request; it
            will not connect automatically when the integration becomes
            available.
          </DialogDescription>
        </DialogHeader>
        <Settings
          embedded
          title="Access lifetime"
          saving={save.isPending}
          saveLabel="Save access request"
          errors={{ 'Lifetime (seconds)': form.formState.errors.ttl?.message }}
          onSubmit={form.handleSubmit((values) => save.mutate(values))}
        >
          <fieldset disabled={save.isPending} className="space-y-4">
            {save.isError && (
              <Callout tone="error" title="Could not save request">
                {problemDetail(
                  save.error,
                  'Reload the service and check your permissions before trying again.'
                )}
              </Callout>
            )}
            <p className="break-all text-sm">
              {service?.policy.origin} · {service?.policy.methods.join(', ')} ·{' '}
              {service?.policy.path_prefixes.join(', ')}
            </p>
            <Field
              label="Lifetime (seconds)"
              error={form.formState.errors.ttl?.message}
              description={`Maximum ${service?.max_ttl_seconds} seconds. Expiry starts when this request is saved.`}
            >
              {(props) => (
                <Input
                  {...form.register('ttl', { valueAsNumber: true })}
                  {...props}
                  type="number"
                  min={60}
                  max={service?.max_ttl_seconds ?? 86400}
                />
              )}
            </Field>
            <p className="text-xs text-muted-foreground">
              No sandbox connection will be created.
            </p>
          </fieldset>
        </Settings>
      </DialogContent>
    </Dialog>
  )
}
