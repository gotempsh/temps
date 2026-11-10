// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
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

export function VaultLinks({ vaultId }: { vaultId: string }) {
  const client = useQueryClient()
  const [page, setPage] = useState(1)
  const [kind, setKind] = useState<'project' | 'sandbox'>('project')
  const [selected, setSelected] = useState<number | string>()
  const [open, setOpen] = useState(false)
  const [removing, setRemoving] = useState<VaultLinkResponse>()
  const links = useQuery(
    listVaultLinksOptions({
      path: { vault_id: vaultId },
      query: { page, page_size: 20 },
    })
  )
  const refresh = () => {
    void client.invalidateQueries({ queryKey: [{ _id: 'listVaultLinks' }] })
    void client.invalidateQueries({
      queryKey: [{ _id: 'listAccessSessionsGeneral' }],
    })
  }
  const add = useMutation({
    mutationFn: () =>
      createVaultLink({
        path: { vault_id: vaultId },
        body:
          kind === 'project'
            ? { project_id: Number(selected) }
            : { sandbox_id: String(selected) },
        throwOnError: true,
      }),
    onSuccess: () => {
      refresh()
      setOpen(false)
      setSelected(undefined)
    },
  })
  const remove = useMutation({
    mutationFn: (link: VaultLinkResponse) =>
      deleteVaultLink({
        path: { vault_id: vaultId, link_id: link.id },
        throwOnError: true,
      }),
    onSuccess: () => {
      refresh()
      setRemoving(undefined)
    },
  })
  return (
    <section className="space-y-3" aria-label="Linked to">
      <h2 className="font-semibold">Linked to</h2>
      <Button variant="outline" onClick={() => setOpen(true)}>
        Link project or sandbox
      </Button>
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
      ) : links.data?.items.length === 0 ? (
        <p>
          No consumers linked. Link a project or sandbox before approving
          sessions.
        </p>
      ) : (
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
          onPageChange={setPage}
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
          <div className="flex gap-2">
            {(['project', 'sandbox'] as const).map((value) => (
              <Button
                key={value}
                variant={kind === value ? 'default' : 'outline'}
                onClick={() => {
                  setKind(value)
                  setSelected(undefined)
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
            onSelect={setSelected}
          />
          {add.isError && (
            <Callout tone="error" title="Could not link consumer">
              {problemDetail(
                add.error,
                'Check the selected consumer and retry.'
              )}
            </Callout>
          )}
          <Button
            disabled={!selected || add.isPending}
            aria-busy={add.isPending}
            onClick={() => add.mutate()}
          >
            Link consumer
          </Button>
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
          if (removing) remove.mutate(removing)
        }}
      />
    </section>
  )
}
