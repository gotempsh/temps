// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useMemo, useState } from 'react'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { useDebounce } from '@/hooks/useDebounce'
import {
  Callout,
  CredentialProviderMark,
  ResponsivePagination,
} from '@temps-sdk/ds'
import { Button, Skeleton } from '@temps-sdk/ui'
import { serviceCatalogOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ServiceCatalogEntry } from '@/api/client'
import { ListToolbar } from '@/components/layout/ListToolbar'
import { ChevronRight } from 'lucide-react'
import { problemDetail } from '@/lib/api-problem'

export function ServiceCatalogPicker({
  selectedId,
  disabled,
  onSelect,
  searchValue,
  pageValue,
  onSearchChange,
  onPageChange,
}: {
  searchValue?: string
  pageValue?: number
  onSearchChange?: (value: string) => void
  onPageChange?: (value: number) => void
  selectedId: string
  disabled: boolean
  onSelect: (entry: ServiceCatalogEntry) => void
}) {
  const [localSearch, setSearch] = useState('')
  const search = searchValue ?? localSearch
  const [localPage, setPage] = useState(1)
  const page = pageValue ?? localPage
  const filters = useMemo(() => ({ page, search }), [page, search])
  const settledFilters = useDebounce(filters, 250)
  const catalog = useQuery({
    ...serviceCatalogOptions({ query: { ...settledFilters, page_size: 20 } }),
    placeholderData: keepPreviousData,
  })
  const updating = filters !== settledFilters || catalog.isFetching
  return (
    <section className="space-y-3" aria-label="Service catalog">
      <ListToolbar
        searchLabel="Search services"
        placeholder="Search services or categories…"
        value={search}
        onChange={(value) => {
          if (onSearchChange) {
            onSearchChange(value)
          } else {
            setSearch(value)
          }
          setPage(1)
        }}
        summary={
          updating && !catalog.isPending
            ? 'Updating services…'
            : `${catalog.data?.total ?? 0} services`
        }
      />
      {catalog.isError && (
        <Callout tone="error" title="Could not load service catalog">
          {problemDetail(catalog.error, 'Could not load the service catalog')}{' '}
          <Button
            type="button"
            variant="outline"
            onClick={() => void catalog.refetch()}
          >
            Retry
          </Button>
        </Callout>
      )}
      {catalog.isPending ? (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-24 rounded-md" />
          ))}
        </div>
      ) : (
        <div
          role="group"
          aria-label="Service provider"
          aria-busy={updating}
          className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3"
        >
          {[
            ...(catalog.data?.items ?? []),
            {
              id: 'custom',
              name: 'Custom HTTP service',
              description: 'Connect an HTTP API with a bearer token',
              category: 'Custom',
              logo: 'custom',
              popular: false,
              supported: true,
              reason: null,
            },
          ].map((entry) => (
            <button
              key={entry.id}
              type="button"
              aria-pressed={selectedId === entry.id}
              disabled={disabled}
              className={`group flex min-h-32 flex-col justify-between gap-4 rounded-xl border bg-card p-4 text-left text-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60 ${selectedId === entry.id ? 'border-foreground bg-muted' : 'hover:border-foreground/20 hover:bg-accent/70'}`}
              onClick={() => onSelect(entry)}
            >
              <span className="flex w-full items-center gap-3">
                <CredentialProviderMark provider={entry.logo ?? entry.id} />
                <span className="min-w-0 flex-1 break-words">
                  <span className="block font-medium">{entry.name}</span>
                  <span className="block text-xs text-muted-foreground">
                    {entry.category}
                  </span>
                </span>
                <ChevronRight
                  aria-hidden="true"
                  className="size-4 shrink-0 text-muted-foreground"
                />
              </span>
              <span className="flex w-full items-center justify-between gap-2 border-t pt-3 text-xs text-muted-foreground">
                <span title={entry.reason ?? undefined}>
                  {entry.supported
                    ? entry.id === 'custom'
                      ? 'Bearer token · HTTP API'
                      : 'Ready to configure'
                    : 'Not supported yet'}
                </span>
                {entry.popular && (
                  <span className="rounded-md bg-muted px-2 py-0.5">
                    Popular
                  </span>
                )}
              </span>
            </button>
          ))}
        </div>
      )}
      {catalog.data?.total === 0 && (
        <p className="text-sm text-muted-foreground">
          No services match this search. You can configure a custom HTTP service
          using the Custom HTTP service card.
        </p>
      )}
      {(catalog.data?.total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={catalog.data?.total ?? 0}
          totalPages={Math.ceil((catalog.data?.total ?? 0) / 20)}
          onPageChange={onPageChange ?? setPage}
        />
      )}
    </section>
  )
}
