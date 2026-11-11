// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useCallback, useEffect, useState } from 'react'
import { useSearchParams } from 'react-router'
import { useForm, useWatch } from 'react-hook-form'
import { zodResolver } from '@hookform/resolvers/zod'
import { z } from 'zod'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Button,
  Skeleton,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@temps-sdk/ui'
import { Callout, DataTable, ResponsivePagination } from '@temps-sdk/ds'
import {
  createVaultLink,
  deleteVaultLink,
  type VaultLinkResponse,
} from '@/api/client'
import { listVaultLinksOptions } from '@/api/client/@tanstack/react-query.gen'
import { VaultConsumerPicker } from './VaultConsumerPicker'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { problemDetail } from '@/lib/api-problem'

const consumerSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('project'),
    selected: z.number({ error: 'Select a project.' }).int().positive(),
  }),
  z.object({
    kind: z.literal('sandbox'),
    selected: z
      .string({ error: 'Select a sandbox.' })
      .trim()
      .min(1, 'Select a sandbox.'),
  }),
])

export function VaultLinks({
  vaultId,
  active = true,
}: {
  vaultId: string
  active?: boolean
}) {
  const client = useQueryClient()
  const [search, setSearch] = useSearchParams()
  const requestedPage = Number(search.get('links_page'))
  const page =
    Number.isSafeInteger(requestedPage) && requestedPage > 0
      ? Math.min(requestedPage, 1_000_000)
      : 1
  const setPage = useCallback(
    (nextPage: number, replace = false) => {
      setSearch(
        (current) => {
          const next = new URLSearchParams(current)
          if (nextPage === 1) next.delete('links_page')
          else next.set('links_page', String(nextPage))
          return next
        },
        { replace }
      )
    },
    [setSearch]
  )
  const form = useForm<z.infer<typeof consumerSchema>>({
    resolver: zodResolver(consumerSchema),
    defaultValues: { kind: 'project' },
  })
  const kind = useWatch({ control: form.control, name: 'kind' })
  const selected = useWatch({ control: form.control, name: 'selected' })
  const [open, setOpen] = useState(false)
  const [removing, setRemoving] = useState<VaultLinkResponse>()
  const links = useQuery({
    ...listVaultLinksOptions({
      path: { vault_id: vaultId },
      query: { page, page_size: 20 },
    }),
    enabled: active,
  })
  useEffect(() => {
    if (!active || !links.isSuccess) return
    const lastPage = Math.max(1, Math.ceil(links.data.total / 20))
    if (page > lastPage) setPage(lastPage, true)
  }, [active, links.isSuccess, links.data?.total, page, setPage])
  const refresh = () => {
    void client.invalidateQueries({ queryKey: [{ _id: 'listVaultLinks' }] })
    void client.invalidateQueries({
      queryKey: [{ _id: 'listAccessSessionsGeneral' }],
    })
  }
  const add = useMutation({
    mutationFn: (values: z.infer<typeof consumerSchema>) =>
      createVaultLink({
        path: { vault_id: vaultId },
        body:
          values.kind === 'project'
            ? { project_id: values.selected }
            : { sandbox_id: values.selected },
        throwOnError: true,
      }),
    onSuccess: () => {
      refresh()
      setOpen(false)
      form.reset({ kind: 'project' })
    },
  })
  const remove = useMutation({
    mutationFn: (link: VaultLinkResponse) =>
      deleteVaultLink({
        path: { vault_id: vaultId, link_id: link.id },
        throwOnError: true,
      }),
    onSuccess: refresh,
  })
  return (
    <section className="space-y-3" aria-label="Linked to">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <h2 className="text-sm font-medium">Linked to</h2>
        {!links.isPending && !links.isError && links.data?.total === 0 && (
          <p className="text-sm text-muted-foreground">No consumers linked</p>
        )}
        <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
          Link project or sandbox
        </Button>
      </div>
      {links.isError && (
        <Callout tone="error" title="Could not load links">
          <Button onClick={() => void links.refetch()}>Retry</Button>
        </Callout>
      )}
      {links.isPending ? (
        <div role="status" aria-label="Loading links" className="space-y-3">
          {[0, 1, 2].map((row) => (
            <div key={row}>
              <div className="flex items-center justify-between border-b p-3">
                <Skeleton className="h-5 w-40" />
                <Skeleton className="h-9 w-16" />
              </div>
            </div>
          ))}
        </div>
      ) : links.data?.items.length === 0 ? null : (
        <DataTable
          aria-label="Vault links"
          rows={links.data?.items ?? []}
          rowKey={(row) => row.id}
          columns={[
            {
              key: 'consumer',
              header: 'Consumer',
              render: (row) =>
                row.consumer_name ??
                (row.project_id
                  ? `Project ${row.project_id}`
                  : `Sandbox ${row.sandbox_id}`),
            },
            {
              key: 'action',
              header: '',
              render: (row) => (
                <Button variant="outline" onClick={() => setRemoving(row)}>
                  Unlink
                </Button>
              ),
            },
          ]}
        />
      )}
      {(links.data?.total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={links.data?.total ?? 0}
          totalPages={Math.ceil((links.data?.total ?? 0) / 20)}
          onPageChange={(nextPage) => setPage(nextPage)}
        />
      )}
      <Dialog
        open={open}
        onOpenChange={(next) => {
          if (!add.isPending) setOpen(next)
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Link a consumer</DialogTitle>
          </DialogHeader>
          <form
            className="space-y-3"
            onSubmit={form.handleSubmit((values) => {
              if (!add.isPending) add.mutate(values)
            })}
          >
            <fieldset disabled={add.isPending} className="space-y-3">
              <legend className="sr-only">Consumer</legend>
              <div className="flex gap-2">
                {(['project', 'sandbox'] as const).map((value) => (
                  <Button
                    key={value}
                    type="button"
                    variant={kind === value ? 'default' : 'outline'}
                    onClick={() => {
                      form.reset(
                        value === 'project'
                          ? { kind: 'project' }
                          : { kind: 'sandbox' }
                      )
                    }}
                  >
                    {value === 'project' ? 'Project' : 'Sandbox'}
                  </Button>
                ))}
              </div>
              <VaultConsumerPicker
                key={kind}
                kind={kind}
                selected={selected}
                onSelect={(id) =>
                  form.setValue('selected', id, {
                    shouldDirty: true,
                    shouldValidate: true,
                  })
                }
              />
            </fieldset>
            {form.formState.errors.selected && (
              <p role="alert" className="text-sm text-destructive">
                {form.formState.errors.selected.message}
              </p>
            )}
            {add.isError && (
              <Callout tone="error" title="Could not link consumer">
                {problemDetail(
                  add.error,
                  'Check the selected consumer and retry.'
                )}
              </Callout>
            )}
            <Button
              type="submit"
              disabled={add.isPending}
              aria-busy={add.isPending}
            >
              Link consumer
            </Button>
          </form>
        </DialogContent>
      </Dialog>
      <ConfirmDeleteDialog
        open={!!removing}
        onOpenChange={(next) => {
          if (!next) setRemoving(undefined)
        }}
        resourceType="consumer"
        resourceName={removing?.consumer_name ?? removing?.id ?? ''}
        verb="Unlink"
        confirmLabel="Unlink and revoke access"
        isPending={remove.isPending}
        consequences={
          <>
            <p>
              Live sessions and grants approved through this link will be
              revoked.
            </p>
            {remove.isError && (
              <Callout tone="error" title="Could not unlink consumer">
                {problemDetail(remove.error, 'Retry unlinking.')}
              </Callout>
            )}
          </>
        }
        onConfirm={() => {
          if (removing)
            remove.mutate(removing, {
              onSuccess: () => setRemoving(undefined),
            })
        }}
      />
    </section>
  )
}
