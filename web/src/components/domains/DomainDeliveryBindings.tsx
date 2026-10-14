// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  deleteDomainDeliveryBinding,
  listDomainDeliveryBindings,
  type DomainDeliveryBindingResponse,
} from '@/api/client'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { ResponsivePagination } from '@/components/ui/responsive-pagination'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'
import { fmtDateTime } from '@temps-sdk/ds'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { toast } from 'sonner'
import { ConfirmDeleteDialog } from '@/components/ui/confirm-delete-dialog'
import { Link } from 'react-router'
import { deliveryError, requireDeliveryData } from './delivery-errors'
import { deliveryPageCount } from './delivery-queries'

const BINDINGS_PAGE_SIZE = 20

export function DomainDeliveryBindings({
  projectId,
  onConfigure,
}: {
  projectId: number
  onConfigure: (
    hostname: string,
    environmentId: number,
    binding?: DomainDeliveryBindingResponse
  ) => void
}) {
  const client = useQueryClient()
  const [removing, setRemoving] =
    useState<DomainDeliveryBindingResponse | null>(null)
  const remove = useMutation({
    mutationFn: async (bindingId: number) => {
      const response = await deleteDomainDeliveryBinding({
        path: { project_id: projectId, binding_id: bindingId },
      })
      if (response.error) throw new Error(deliveryError(response.error))
    },
    onSuccess: () => {
      setRemoving(null)
      toast.success('Managed DNS removed. The domain route is preserved.')
    },
    onError: (error) => {
      toast.error(`Could not remove managed DNS: ${deliveryError(error)}`)
    },
    onSettled: () =>
      client.invalidateQueries({ queryKey: ['delivery-bindings', projectId] }),
  })
  const [page, setPage] = useState(1)
  const listQuery = {
    page,
    page_size: BINDINGS_PAGE_SIZE,
    sort_by: 'created_at',
    sort_order: 'desc',
  }
  const bindings = useQuery({
    // Mutations invalidate ['delivery-bindings', projectId], which covers
    // every page of this project.
    queryKey: ['delivery-bindings', projectId, listQuery],
    queryFn: async () =>
      requireDeliveryData(
        await listDomainDeliveryBindings({
          path: { project_id: projectId },
          query: listQuery,
        })
      ),
    // Keep this project's rows on screen while another page loads, but never
    // show one project's bindings under another.
    placeholderData: (previous, previousQuery) =>
      previousQuery?.queryKey[1] === projectId ? previous : undefined,
  })
  const total = bindings.data?.total ?? 0
  const totalPages = deliveryPageCount(total, BINDINGS_PAGE_SIZE)
  // Removing the last binding on the last page leaves it past the end: move
  // to the last page that still has rows rather than showing an empty one.
  if (bindings.data && !bindings.isPlaceholderData && page > totalPages)
    setPage(totalPages)
  const rows = bindings.data?.items ?? []
  const loadingRows = bindings.isPending || (total > 0 && rows.length === 0)
  return (
    <section aria-labelledby="delivery-bindings-title" className="space-y-3">
      <h3 id="delivery-bindings-title" className="font-semibold">
        Managed delivery
      </h3>
      {bindings.isError && (
        <Alert variant="destructive">
          <AlertDescription>
            {deliveryError(bindings.error)}{' '}
            <Button variant="link" onClick={() => bindings.refetch()}>
              Retry
            </Button>
          </AlertDescription>
        </Alert>
      )}
      {loadingRows ? (
        <Skeleton className="h-24 w-full" />
      ) : rows.length > 0 ? (
        <ul
          className={cn(
            'divide-y rounded-lg border',
            bindings.isPlaceholderData && 'opacity-60 transition-opacity'
          )}
          aria-busy={bindings.isPlaceholderData}
        >
          {rows.map((binding) => (
            <li key={binding.id} className="space-y-3 p-4">
              <div className="flex flex-col justify-between gap-3 sm:flex-row sm:items-start">
                <div className="min-w-0">
                  <p className="break-all font-medium">{binding.hostname}</p>
                  <p className="mt-1 text-sm text-muted-foreground">
                    {binding.delivery_profile_name} · {binding.origin_target}
                  </p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Applied from {binding.profile_source.replace(/_/g, ' ')}.
                    Defaults do not change this binding automatically.
                  </p>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Last changed{' '}
                    <time dateTime={binding.updated_at}>
                      {fmtDateTime(binding.updated_at)}
                    </time>
                  </p>
                  {binding.status === 'dns_configured' && (
                    <p className="mt-2 text-xs text-muted-foreground">
                      DNS is configured. Verify public HTTPS before sending
                      traffic.{' '}
                      <Link className="underline" to="/certificates">
                        Manage origin certificates
                      </Link>
                    </p>
                  )}
                </div>
                <div className="flex shrink-0 flex-wrap items-center gap-3">
                  <Badge
                    variant={binding.last_error ? 'destructive' : 'secondary'}
                  >
                    {binding.status.replace(/_/g, ' ')}
                  </Badge>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() =>
                      onConfigure(
                        binding.hostname,
                        binding.environment_id,
                        binding
                      )
                    }
                  >
                    {binding.last_error ? 'Review and retry' : 'Change setup'}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={binding.status === 'applying'}
                    onClick={() => {
                      remove.reset()
                      setRemoving(binding)
                    }}
                  >
                    Remove managed DNS
                  </Button>
                  {binding.status === 'dns_configured' && (
                    <Button size="sm" variant="ghost" asChild>
                      <a
                        href={`https://${binding.hostname}`}
                        target="_blank"
                        rel="noreferrer"
                      >
                        Open HTTPS
                      </a>
                    </Button>
                  )}
                </div>
              </div>
              {binding.last_error && (
                <Alert variant="destructive">
                  <AlertDescription>{binding.last_error}</AlertDescription>
                </Alert>
              )}
            </li>
          ))}
        </ul>
      ) : bindings.data ? (
        <p className="rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          No managed delivery bindings yet. Configure a hostname to inspect its
          DNS records and choose how traffic reaches this project.
        </p>
      ) : null}
      {bindings.data && totalPages > 1 && (
        <ResponsivePagination
          page={page}
          pageSize={BINDINGS_PAGE_SIZE}
          total={total}
          totalPages={totalPages}
          onPageChange={setPage}
          ariaLabel="Managed delivery pagination"
        />
      )}
      <ConfirmDeleteDialog
        open={removing !== null}
        onOpenChange={(open) => {
          if (!open) setRemoving(null)
        }}
        resourceType="managed DNS for"
        resourceName={removing?.hostname ?? ''}
        verb="Remove"
        confirmLabel="Remove managed DNS"
        consequences={removeConsequences(removing)}
        irreversible={false}
        isPending={remove.isPending}
        onConfirm={() => {
          if (removing) remove.mutate(removing.id)
        }}
      />
    </section>
  )
}

function removeConsequences(
  binding: DomainDeliveryBindingResponse | null
): string {
  const base =
    'This removes the owned DNS record and can interrupt traffic. The domain route and certificate stay in Temps. You can delete the domain route afterward.'
  if (binding?.provider_kind !== 'bunny') return base
  if (binding.bunny_hostname_owned) {
    return `${base} Temps added ${binding.hostname} to your Bunny Pull Zone, so it is also detached from the Pull Zone, together with its edge certificate.`
  }
  return `${base} ${binding.hostname} was on your Bunny Pull Zone before Temps set up delivery, so it stays there with its edge certificate. Remove it in Bunny if you no longer need it.`
}
