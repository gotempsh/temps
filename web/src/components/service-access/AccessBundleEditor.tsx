// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useEffect, useRef, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { Checkbox, Input } from '@temps-sdk/ui'
import { Callout, Field, Settings } from '@temps-sdk/ds'
import {
  createAccessBundleGeneral,
  updateAccessBundleGeneral,
  type AccessBundleResponse,
  type AccessServiceResponse,
} from '@/api/client'
import { ServiceProviderLogo } from './service-providers'
import { problemDetail } from '@/lib/api-problem'
const schema = z.object({
  name: z.string().trim().min(1).max(80),
  service_ids: z
    .array(z.string())
    .min(1, 'Select at least one service')
    .max(16, 'Select at most 16 services'),
  ttl_seconds: z.number().int().min(60).max(86400),
})
export function AccessBundleEditor({
  vaultId,
  services,
  onClose,
  canWrite,
  initialBundle,
}: {
  vaultId: string
  services: AccessServiceResponse[]
  onClose: () => void
  canWrite: boolean
  initialBundle?: AccessBundleResponse
}) {
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])
  const client = useQueryClient()
  const [initialSnapshot] = useState(initialBundle)
  const form = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: initialSnapshot
      ? {
          name: initialSnapshot.name,
          service_ids: initialSnapshot.service_ids,
          ttl_seconds: initialSnapshot.ttl_seconds,
        }
      : { name: '', service_ids: [], ttl_seconds: 900 },
  })
  const selected = useWatch({ control: form.control, name: 'service_ids' })
  const save = useMutation({
    mutationFn: async (values: z.infer<typeof schema>) => {
      if (!canWrite) throw new Error('Project write permission required')
      if (initialSnapshot)
        return updateAccessBundleGeneral({
          path: {
            vault_id: vaultId,
            bundle_id: initialSnapshot.id,
          },
          body: { ...values, expected_version: initialSnapshot.version },
          throwOnError: true,
        })
      return createAccessBundleGeneral({
        path: { vault_id: vaultId },
        body: values,
        throwOnError: true,
      })
    },
    onSuccess: () => {
      void client.invalidateQueries({
        queryKey: [{ _id: 'listAccessBundlesGeneral' }],
      })
      if (!initialSnapshot) form.reset()
      if (mounted.current) onClose()
    },
  })
  return (
    <Settings
      title={initialSnapshot ? 'Edit access bundle' : 'Create access bundle'}
      onSubmit={form.handleSubmit((v) => save.mutate(v))}
      onCancel={() => {
        if (!save.isPending) onClose()
      }}
      saving={save.isPending}
      dirty={canWrite}
      saveLabel="Save access bundle"
      errors={{
        'Bundle name': form.formState.errors.name?.message,
        'Access lifetime': form.formState.errors.ttl_seconds?.message,
        Services: form.formState.errors.service_ids?.message,
      }}
    >
      <fieldset disabled={save.isPending} className="space-y-4">
        <Field label="Bundle name" error={form.formState.errors.name?.message}>
          {(props) => <Input {...form.register('name')} {...props} />}
        </Field>
        <Field
          label="Access lifetime (seconds)"
          error={form.formState.errors.ttl_seconds?.message}
        >
          {(props) => (
            <Input
              type="number"
              min={60}
              max={86400}
              {...form.register('ttl_seconds', { valueAsNumber: true })}
              {...props}
            />
          )}
        </Field>
        <fieldset className="space-y-2">
          <legend className="text-sm font-medium">Services</legend>
          {services.map((s) => (
            <label
              key={s.id}
              className="flex items-center gap-3 rounded-md border p-3"
            >
              <Checkbox
                checked={selected.includes(s.id)}
                onCheckedChange={(checked) =>
                  form.setValue(
                    'service_ids',
                    checked
                      ? [...selected, s.id]
                      : selected.filter((id) => id !== s.id),
                    { shouldDirty: true, shouldValidate: true }
                  )
                }
              />
              <ServiceProviderLogo service={s} />
              <span className="flex-1 text-sm">{s.name}</span>
              <span className="text-xs text-muted-foreground">
                Maximum {s.max_ttl_seconds / 60} min
              </span>
            </label>
          ))}
        </fieldset>
        {form.formState.errors.service_ids && (
          <p role="alert" className="text-sm text-destructive">
            {form.formState.errors.service_ids.message}
          </p>
        )}
        {save.isError && (
          <Callout tone="error" title="Could not save access bundle">
            {problemDetail(
              save.error,
              'Check the selected services and lifetime.'
            )}
          </Callout>
        )}
      </fieldset>
    </Settings>
  )
}
