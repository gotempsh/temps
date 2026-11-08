// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { Controller, useForm } from 'react-hook-form'
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
  Textarea,
  Checkbox,
} from '@temps-sdk/ui'
import { toast } from 'sonner'
import {
  createAccessService,
  updateAccessService,
  type AccessServiceResponse,
} from '@/api/client'
import { problemDetail } from '@/lib/api-problem'
import {
  accessServiceSchema,
  serviceDefaults,
  serviceBody,
  type AccessServiceValues,
} from './service-access-form'

export function AccessServiceEditor({
  projectId,
  open,
  service,
  onClose,
  onSaved,
}: {
  projectId?: number | null
  open: boolean
  service?: AccessServiceResponse
  onClose: () => void
  onSaved: () => void
}) {
  const form = useForm<AccessServiceValues>({
    resolver: zodResolver(accessServiceSchema),
    defaultValues: serviceDefaults(),
  })
  const save = useMutation({
    mutationFn: async (values: AccessServiceValues) => {
      if (!projectId)
        throw new Error('Select a project before saving a service')
      const body = serviceBody(values, service?.version)
      if (service)
        return updateAccessService({
          path: { project_id: projectId, service_id: service.id },
          body,
          throwOnError: true,
        })
      return createAccessService({
        path: { project_id: projectId },
        body,
        throwOnError: true,
      })
    },
    onSuccess: () => {
      form.reset(serviceDefaults())
      toast.success('Service saved', {
        description:
          'Sandbox connections remain unavailable until the integration is installed.',
      })
      onSaved()
      onClose()
    },
  })
  const resetMutation = save.reset
  useEffect(() => {
    form.reset(serviceDefaults(service))
    resetMutation()
  }, [open, service, form, resetMutation])

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !save.isPending) onClose()
      }}
    >
      <DialogContent className="max-h-[90dvh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{service ? 'Edit service' : 'Add service'}</DialogTitle>
          <DialogDescription>
            Save an HTTPS policy and optional bearer credential. This does not
            connect a sandbox.
          </DialogDescription>
        </DialogHeader>
        <Settings
          embedded
          title="Service policy"
          onSubmit={form.handleSubmit((values) => save.mutate(values))}
          dirty={form.formState.isDirty && !!projectId}
          saving={save.isPending}
          saveLabel="Save service"
          errors={{
            Name: form.formState.errors.name?.message,
            'HTTPS origin': form.formState.errors.origin?.message,
            'Allowed path prefixes': form.formState.errors.paths?.message,
            'Allowed HTTP methods': form.formState.errors.methods?.message,
            'Maximum access lifetime': form.formState.errors.maxTtl?.message,
            'Bearer credential': form.formState.errors.credential?.message,
          }}
        >
          <fieldset disabled={save.isPending} className="space-y-4">
            {save.isError && (
              <Callout tone="error" title="Could not save service">
                {problemDetail(
                  save.error,
                  'Check your permissions and reload the service before trying again.'
                )}
              </Callout>
            )}
            <Field label="Name" error={form.formState.errors.name?.message}>
              {(props) => (
                <Input
                  {...form.register('name')}
                  {...props}
                  placeholder="Repository API"
                />
              )}
            </Field>
            <Field
              label="HTTPS origin"
              error={form.formState.errors.origin?.message}
            >
              {(props) => (
                <Input
                  {...form.register('origin')}
                  {...props}
                  placeholder="https://api.example.com"
                />
              )}
            </Field>
            <Field
              label="Allowed path prefixes"
              error={form.formState.errors.paths?.message}
              description="One prefix per line. / allows all paths on this origin."
            >
              {(props) => (
                <Textarea
                  {...form.register('paths')}
                  {...props}
                  placeholder="/v1"
                />
              )}
            </Field>
            <Controller
              control={form.control}
              name="methods"
              render={({ field, fieldState }) => (
                <Field
                  label="Allowed HTTP methods"
                  error={fieldState.error?.message}
                >
                  {(props) => (
                    <div
                      {...props}
                      role="group"
                      className="flex flex-wrap gap-4"
                    >
                      {(
                        [
                          'GET',
                          'HEAD',
                          'POST',
                          'PUT',
                          'PATCH',
                          'DELETE',
                        ] as const
                      ).map((method) => (
                        <label
                          key={method}
                          className="flex items-center gap-2 text-sm"
                        >
                          <Checkbox
                            checked={field.value.includes(method)}
                            onCheckedChange={(checked) =>
                              field.onChange(
                                checked
                                  ? [...field.value, method]
                                  : field.value.filter(
                                      (value) => value !== method
                                    )
                              )
                            }
                          />
                          {method}
                        </label>
                      ))}
                    </div>
                  )}
                </Field>
              )}
            />
            <Field
              label="Maximum access lifetime (seconds)"
              error={form.formState.errors.maxTtl?.message}
            >
              {(props) => (
                <Input
                  {...form.register('maxTtl', { valueAsNumber: true })}
                  {...props}
                  type="number"
                  min={60}
                  max={86400}
                />
              )}
            </Field>
            <Field
              label={
                service?.has_credential
                  ? 'Replace bearer credential'
                  : 'Bearer credential (optional)'
              }
              error={form.formState.errors.credential?.message}
              description="Encrypted when saved; never displayed again. Leave empty to keep the existing credential."
            >
              {(props) => (
                <Input
                  {...form.register('credential')}
                  {...props}
                  type="password"
                  autoComplete="new-password"
                />
              )}
            </Field>
            <Controller
              control={form.control}
              name="clearCredential"
              render={({ field, fieldState }) => (
                <Field
                  label="Remove stored credential"
                  error={fieldState.error?.message}
                >
                  {(props) => (
                    <Checkbox
                      {...props}
                      checked={field.value}
                      onCheckedChange={(checked) =>
                        field.onChange(checked === true)
                      }
                    />
                  )}
                </Field>
              )}
            />
            <Controller
              control={form.control}
              name="enabled"
              render={({ field, fieldState }) => (
                <Field
                  label="Allow new access requests"
                  error={fieldState.error?.message}
                >
                  {(props) => (
                    <Checkbox
                      {...props}
                      checked={field.value}
                      onCheckedChange={(checked) =>
                        field.onChange(checked === true)
                      }
                    />
                  )}
                </Field>
              )}
            />
            {service && (
              <p className="text-xs text-muted-foreground">
                Saving a change invalidates requests for the previous service
                version.
              </p>
            )}
            <p className="text-xs text-muted-foreground">
              Saving does not enable network access.
            </p>
          </fieldset>
        </Settings>
      </DialogContent>
    </Dialog>
  )
}
