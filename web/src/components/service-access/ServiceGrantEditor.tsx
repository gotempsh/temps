// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect, useRef } from 'react'
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
import {
  approveServiceGrant,
  requestServiceGrant,
  type AccessServiceResponse,
} from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import { grantRequestSchema } from './service-access-form'

export function ServiceGrantEditor({
  projectId,
  sandboxId,
  service,
  onClose,
  onSaved,
  mode = 'request',
  defaultTtl = 900,
}: {
  defaultTtl?: number
  projectId?: number | null
  sandboxId: string
  mode?: 'request' | 'approve'
  service?: AccessServiceResponse
  onClose: () => void
  onSaved: () => void
}) {
  const mounted = useRef(true)
  const approval = useRef<{ key: string; id: string } | null>(null)
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])
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
      const path = { project_id: projectId, sandbox_id: sandboxId }
      const body = {
        service_id: service.id,
        service_version: service.version,
        ttl_seconds: ttl,
      }
      if (mode === 'approve') {
        const key = JSON.stringify({ path, body })
        if (approval.current?.key !== key)
          approval.current = { key, id: crypto.randomUUID() }
        return approveServiceGrant({
          path,
          body: { ...body, request_id: approval.current.id },
          throwOnError: true,
        })
      }
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
      onSaved()
      if (!mounted.current) return
      toast.success(
        mode === 'approve' ? 'Access approved' : 'Access request saved',
        {
          description:
            mode === 'approve'
              ? 'Waiting for the assigned worker to confirm access. The lifetime starts now.'
              : 'The sandbox is not connected. Operator approval is required.',
        }
      )
      onClose()
    },
  })
  const resetMutation = save.reset
  useEffect(() => {
    form.reset({ ttl: Math.min(defaultTtl, service?.max_ttl_seconds ?? 900) })
    resetMutation()
    approval.current = null
  }, [service, form, resetMutation, mode, defaultTtl])
  return (
    <Dialog
      open={!!service}
      onOpenChange={(open) => {
        if (!open && !save.isPending) onClose()
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {mode === 'approve' ? 'Approve access to' : 'Request access to'}{' '}
            {service?.name}
          </DialogTitle>
          <DialogDescription>
            {mode === 'approve'
              ? 'Approve the displayed service policy for this sandbox and lifetime. Credentials stay on the host. Access begins only after the worker confirms it.'
              : 'Save a request for operator review. Saving alone does not connect the sandbox or approve access.'}
          </DialogDescription>
        </DialogHeader>
        <Settings
          embedded
          title="Access lifetime"
          saving={save.isPending}
          saveLabel={
            mode === 'approve' ? 'Approve access' : 'Save access request'
          }
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
            <p className="break-all text-sm">Sandbox: {sandboxId}</p>
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
              {mode === 'approve'
                ? 'A new sandbox incarnation requires another approval. You can revoke this grant at any time.'
                : 'No sandbox connection will be created.'}
            </p>
          </fieldset>
        </Settings>
      </DialogContent>
    </Dialog>
  )
}
