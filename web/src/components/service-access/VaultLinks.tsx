// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useCallback, useEffect, useState } from 'react'
import { useSearchParams } from 'react-router'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Button, Skeleton } from '@temps-sdk/ui'
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

type Consumer =
  { kind: 'project'; selected: number } | { kind: 'sandbox'; selected: string }

export function VaultLinks({
  vaultId,
  active = true,
}: {
  vaultId: string
  active?: boolean
}) {
  const client = useQueryClient()
  const [search, setSearch] = useSearchParams()
  const readPage = (key: string) => {
    const value = Number(search.get(key))
    return Number.isSafeInteger(value) && value > 0
      ? Math.min(value, 1_000_000)
      : 1
  }
  const page = readPage('links_page')
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
  // Returned so a mutation stays pending until the link queries, including
  // each picker's membership check, have refreshed: a consumer that was just
  // linked then reads "Linked" instead of offering the same action again.
  const refresh = () =>
    Promise.all([
      client.invalidateQueries({ queryKey: [{ _id: 'listVaultLinks' }] }),
      client.invalidateQueries({
        queryKey: [{ _id: 'listAccessSessionsGeneral' }],
      }),
    ])
  const add = useMutation({
    mutationFn: (values: Consumer) =>
      createVaultLink({
        path: { vault_id: vaultId },
        body:
          values.kind === 'project'
            ? { project_id: values.selected }
            : { sandbox_id: values.selected },
        throwOnError: true,
      }),
    onSuccess: refresh,
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
      <section aria-label="Link a consumer" className="space-y-4">
        <fieldset disabled={add.isPending} className="space-y-3">
          <legend className="sr-only">Consumer</legend>
          <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
            {(['project', 'sandbox'] as const).map((value) => (
              <section
                key={value}
                aria-label={value === 'project' ? 'Projects' : 'Sandboxes'}
                className="min-w-0 space-y-3"
              >
                <h3 className="text-sm font-medium">
                  {value === 'project' ? 'Projects' : 'Sandboxes'}
                </h3>
                <VaultConsumerPicker
                  kind={value}
                  active={active}
                  linkAction
                  pending={add.isPending}
                  vaultId={vaultId}
                  page={readPage(`links_${value}_page`)}
                  onPageChange={(nextPage) =>
                    setSearch((current) => {
                      const next = new URLSearchParams(current)
                      if (nextPage === 1) next.delete(`links_${value}_page`)
                      else next.set(`links_${value}_page`, String(nextPage))
                      return next
                    })
                  }
                  onSelect={(id) => {
                    if (!add.isPending)
                      add.mutate(
                        value === 'project'
                          ? { kind: value, selected: Number(id) }
                          : { kind: value, selected: String(id) }
                      )
                  }}
                />
              </section>
            ))}
          </div>
        </fieldset>
        {add.isError && (
          <Callout tone="error" title="Could not link consumer">
            {problemDetail(add.error, 'Check the selected consumer and retry.')}
          </Callout>
        )}
      </section>
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
