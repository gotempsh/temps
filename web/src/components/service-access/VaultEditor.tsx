// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState } from 'react'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Button,
  Checkbox,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  Input,
} from '@temps-sdk/ui'
import { Callout, Field, ResponsivePagination } from '@temps-sdk/ds'
import {
  getAccessVaultGeneral,
  updateAccessVaultGeneral,
  type AccessVaultResponse,
} from '@/api/client'
import { listAccessServicesGeneralOptions } from '@/api/client/@tanstack/react-query.gen'
import { ServiceProviderLogo } from './service-providers'
import { problemDetail } from '@/lib/api-problem'

const schema = z.object({
  name: z.string().trim().min(1).max(80),
  service_ids: z.array(z.string()).max(100),
})

// The caller supplies the snapshot selected by the user, never a refetch result.
export function VaultEditor({
  vault,
  open,
  onClose,
  canWrite,
}: {
  vault: AccessVaultResponse
  open: boolean
  onClose: () => void
  canWrite: boolean
}) {
  const [snapshot, setSnapshot] = useState(vault)
  const [page, setPage] = useState(1)
  const client = useQueryClient()
  const form = useForm<z.infer<typeof schema>>({
    resolver: zodResolver(schema),
    defaultValues: { name: snapshot.name, service_ids: snapshot.service_ids },
  })
  const selected = useWatch({ control: form.control, name: 'service_ids' })
  const services = useQuery({
    ...listAccessServicesGeneralOptions({
      query: { page, page_size: 20 },
    }),
    enabled: open,
  })
  const latest = useMutation({
    mutationFn: () =>
      getAccessVaultGeneral({
        path: { vault_id: snapshot.id },
        throwOnError: true,
      }),
  })
  const save = useMutation({
    mutationFn: (values: z.infer<typeof schema>) => {
      if (!canWrite)
        throw new Error('Instance administrator permission is required')
      return updateAccessVaultGeneral({
        path: { vault_id: snapshot.id },
        body: { ...values, expected_version: snapshot.version },
        throwOnError: true,
      })
    },
    onSuccess: ({ data: saved }) => {
      // Only a successful save replaces the draft and its concurrency version.
      // Closing the dialog or refetching the page must not reset either one.
      latest.reset()
      setSnapshot(saved)
      form.reset({ name: saved.name, service_ids: saved.service_ids })
      void client.invalidateQueries({
        queryKey: [{ _id: 'getAccessVaultGeneral' }],
      })
      void client.invalidateQueries({
        queryKey: [{ _id: 'listAccessVaultsGeneral' }],
      })
      void client.invalidateQueries({ queryKey: [{ _id: 'getAccessVault' }] })
      void client.invalidateQueries({ queryKey: [{ _id: 'listAccessVaults' }] })
      onClose()
    },
  })
  return (
    <Dialog
      open={open}
      onOpenChange={(value) => {
        if (!value && !save.isPending) onClose()
      }}
    >
      <DialogContent className="max-h-[90dvh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Edit vault</DialogTitle>
        </DialogHeader>
        <form
          className="space-y-4"
          onSubmit={form.handleSubmit((values) => save.mutate(values))}
        >
          <Field label="Vault name" error={form.formState.errors.name?.message}>
            {(props) => (
              <Input
                {...props}
                {...form.register('name')}
                disabled={save.isPending}
              />
            )}
          </Field>
          <fieldset className="space-y-2">
            <legend className="mb-2 text-sm font-medium">
              Services · {selected.length} selected
            </legend>
            {services.data?.items.map((service) => (
              <label
                key={service.id}
                className="flex items-center gap-3 rounded-md border p-3"
              >
                <Checkbox
                  disabled={save.isPending}
                  checked={selected.includes(service.id)}
                  onCheckedChange={(checked) =>
                    !save.isPending &&
                    form.setValue(
                      'service_ids',
                      checked
                        ? [...selected, service.id]
                        : selected.filter((id) => id !== service.id),
                      { shouldDirty: true, shouldValidate: true }
                    )
                  }
                />
                <ServiceProviderLogo service={service} />
                <span className="min-w-0 break-words text-sm">
                  {service.name}
                </span>
              </label>
            ))}
            {services.data?.total === 0 && (
              <p className="text-sm text-muted-foreground">
                No services yet. Use Add service in the vault to create one.
              </p>
            )}
          </fieldset>
          {services.isError && (
            <Callout tone="error" title="Could not load services">
              <Button type="button" onClick={() => void services.refetch()}>
                Retry
              </Button>
            </Callout>
          )}
          {(services.data?.total ?? 0) > 20 && (
            <ResponsivePagination
              page={page}
              pageSize={20}
              total={services.data?.total ?? 0}
              totalPages={Math.ceil((services.data?.total ?? 0) / 20)}
              onPageChange={setPage}
            />
          )}
          {form.formState.errors.service_ids && (
            <p role="alert" className="text-sm text-destructive">
              {form.formState.errors.service_ids.message}
            </p>
          )}
          {save.isError && (
            <Callout tone="error" title="Could not save vault">
              {problemDetail(
                save.error,
                'Check the selected services. Services used by a bundle must remain in the vault.'
              )}
            </Callout>
          )}
          {latest.isError && (
            <Callout tone="error" title="Could not load the latest vault">
              {problemDetail(
                latest.error,
                'Your draft is unchanged. Retry loading the latest version.'
              )}
            </Callout>
          )}
          {latest.data && (
            <Callout title="Latest saved version">
              <p>
                {latest.data.data.vault.name} · Version{' '}
                {latest.data.data.vault.version} ·{' '}
                {latest.data.data.vault.service_ids.length} services
              </p>
              <p>
                Your draft is unchanged. Replacing it discards your unsaved name
                and service selections.
              </p>
              <Button
                type="button"
                variant="outline"
                disabled={save.isPending || latest.isPending}
                onClick={() => {
                  if (save.isPending || latest.isPending || !latest.data) return
                  const current = latest.data.data.vault
                  setSnapshot(current)
                  form.reset({
                    name: current.name,
                    service_ids: current.service_ids,
                  })
                  setPage(1)
                  save.reset()
                  latest.reset()
                }}
              >
                Discard draft and use latest
              </Button>
            </Callout>
          )}
          <div className="flex flex-wrap justify-end gap-2">
            <Button
              type="button"
              variant="outline"
              disabled={save.isPending || latest.isPending}
              onClick={() => {
                if (!save.isPending && !latest.isPending) latest.mutate()
              }}
            >
              {latest.isPending ? 'Loading latest…' : 'Review latest version'}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={save.isPending}
              onClick={onClose}
            >
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={
                !canWrite ||
                latest.isPending ||
                save.isPending ||
                services.isError ||
                services.isPending
              }
            >
              Save vault
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  )
}
