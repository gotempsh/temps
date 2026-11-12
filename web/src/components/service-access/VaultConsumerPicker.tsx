// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { Link } from 'react-router'
import { Boxes, Folder } from 'lucide-react'
import { EmptyState } from '@/components/ui/empty-state'
import { useQuery } from '@tanstack/react-query'
import { Button, Skeleton } from '@temps-sdk/ui'
import { Callout, ResponsivePagination } from '@temps-sdk/ds'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import {
  getProjectsOptions,
  listSandboxesOptions,
  listVaultLinksOptions,
} from '@/api/client/@tanstack/react-query.gen'

export function VaultConsumerPicker({
  kind,
  selected,
  onSelect,
  active = true,
  page: controlledPage,
  onPageChange,
  linkAction = false,
  pending = false,
  vaultId,
}: {
  kind: 'project' | 'sandbox'
  active?: boolean
  linkAction?: boolean
  pending?: boolean
  /** With `linkAction`, the vault whose links decide which rows read "Linked". */
  vaultId?: string
  page?: number
  onPageChange?: (page: number) => void
  selected?: number | string
  onSelect: (id: number | string) => void
}) {
  const [localPage, setLocalPage] = useState(1)
  const page = controlledPage ?? localPage
  const setPage = onPageChange ?? setLocalPage
  const projects = useQuery({
    ...getProjectsOptions({ query: { page, per_page: 20 } }),
    enabled: active && kind === 'project',
  })
  const sandboxes = useQuery({
    ...listSandboxesOptions({ query: { page, page_size: 20 } }),
    enabled: active && kind === 'sandbox',
  })
  const plural = kind === 'project' ? 'projects' : 'sandboxes'
  const query = kind === 'project' ? projects : sandboxes
  const rows =
    kind === 'project'
      ? projects.data?.projects.map((project) => ({
          ...project,
          icon: <ServiceIcon service={project} size="xs" />,
        }))
      : sandboxes.data?.sandboxes.map((sandbox) => ({ ...sandbox, icon: null }))
  const total =
    kind === 'project' ? projects.data?.total : sandboxes.data?.pagination.count
  // Whether a displayed consumer is linked is asked of the server for exactly
  // the rows on screen. The vault's own links table is paged separately, so
  // deriving it from that page marks a linked consumer as unlinked.
  const displayedIds = (rows ?? []).map((row) => String(row.id)).join(',')
  const membership = useQuery({
    ...listVaultLinksOptions({
      path: { vault_id: vaultId ?? '' },
      query: {
        page_size: 100,
        ...(kind === 'project'
          ? { project_ids: displayedIds }
          : { sandbox_ids: displayedIds }),
      },
    }),
    enabled: active && linkAction && !!vaultId && displayedIds !== '',
  })
  const linkedIds = (membership.data?.items ?? []).flatMap<string | number>(
    (link) => {
      const id = kind === 'project' ? link.project_id : link.sandbox_id
      return id == null ? [] : [id]
    }
  )
  const membershipPending = linkAction && !!vaultId && membership.isPending
  // Without the membership answer a linked consumer would read "Link", so
  // every link action waits until the lookup succeeds.
  const membershipFailed = linkAction && !!vaultId && membership.isError
  return (
    <div className="space-y-3">
      {membershipFailed && !query.isError && (
        <Callout
          tone="error"
          title={`Could not check which ${plural} are already linked`}
        >
          <p>Linking is paused until this check succeeds.</p>
          <Button type="button" onClick={() => void membership.refetch()}>
            Retry
          </Button>
        </Callout>
      )}
      {query.isError ? (
        <Callout tone="error" title={`Could not load ${plural}`}>
          <Button type="button" onClick={() => void query.refetch()}>
            Retry
          </Button>
        </Callout>
      ) : query.isPending ? (
        <div role="status" aria-label="Loading consumers" className="space-y-3">
          {[0, 1, 2].map((row) => (
            <div key={row}>
              <Skeleton className="h-12 w-full rounded-md" />
            </div>
          ))}
        </div>
      ) : !rows?.length ? (
        <div className="rounded-lg border bg-card text-card-foreground">
          <EmptyState
            size="compact"
            icon={kind === 'project' ? Folder : Boxes}
            title={
              (total ?? 0) === 0
                ? `No ${plural} yet`
                : `No ${plural} on this page`
            }
            description={
              (total ?? 0) === 0
                ? `Create a ${kind} to link it to this vault.`
                : 'Return to the first page to choose a consumer.'
            }
            action={
              (total ?? 0) === 0 ? (
                <Button asChild variant="outline">
                  <Link to={kind === 'project' ? '/projects' : '/sandboxes'}>
                    {kind === 'project' ? 'View projects' : 'View sandboxes'}
                  </Link>
                </Button>
              ) : (
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => setPage(1)}
                >
                  First page
                </Button>
              )
            }
          />
        </div>
      ) : (
        rows.map((row) =>
          linkAction ? (
            <div
              key={row.id}
              className="flex min-w-0 items-center justify-between gap-3 rounded-lg border bg-card px-4 py-3"
            >
              <div className="flex min-w-0 items-center gap-3">
                {row.icon}
                <span className="break-words text-sm font-medium">
                  {row.name}
                </span>
              </div>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={
                  pending ||
                  membershipPending ||
                  membershipFailed ||
                  linkedIds.includes(row.id)
                }
                aria-label={`${linkedIds.includes(row.id) ? 'Linked' : 'Link'} ${row.name}`}
                onClick={() => onSelect(row.id)}
              >
                {linkedIds.includes(row.id) ? 'Linked' : 'Link'}
              </Button>
            </div>
          ) : (
            <Button
              key={row.id}
              className="flex h-auto min-h-12 w-full justify-start gap-3 whitespace-normal rounded-lg px-4 py-3 text-left"
              type="button"
              variant={selected === row.id ? 'default' : 'outline'}
              aria-pressed={selected === row.id}
              onClick={() => onSelect(row.id)}
            >
              {row.icon}
              <span className="min-w-0 break-words">{row.name}</span>
            </Button>
          )
        )
      )}
      {(total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={total ?? 0}
          totalPages={Math.ceil((total ?? 0) / 20)}
          onPageChange={setPage}
        />
      )}
    </div>
  )
}
