// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  Callout,
  CredentialProviderMark,
  ResponsivePagination,
} from '@temps-sdk/ds'
import { Button, Input, Skeleton } from '@temps-sdk/ui'
import { serviceCatalogOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ServiceCatalogEntry } from '@/api/client'
import { problemDetail } from '@/lib/api-problem'

export function ServiceCatalogPicker({
  selectedId,
  disabled,
  onSelect,
}: {
  selectedId: string
  disabled: boolean
  onSelect: (entry: ServiceCatalogEntry) => void
}) {
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(1)
  const catalog = useQuery(
    serviceCatalogOptions({ query: { page, page_size: 20, search } })
  )
  return (
    <section className="space-y-3" aria-label="Service catalog">
      <Input
        aria-label="Search services"
        placeholder="Search services or categories"
        value={search}
        onChange={(e) => {
          setSearch(e.target.value)
          setPage(1)
        }}
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
        <div className="grid grid-cols-1 gap-2 lg:grid-cols-3">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-24 rounded-md" />
          ))}
        </div>
      ) : (
        <div
          role="group"
          aria-label="Service provider"
          className="grid grid-cols-1 gap-2 lg:grid-cols-3"
        >
          {catalog.data?.items.map((entry) => (
            <button
              key={entry.id}
              type="button"
              aria-pressed={selectedId === entry.id}
              disabled={disabled || !entry.supported}
              className={`flex items-start gap-3 rounded-md border p-3 text-left text-sm disabled:opacity-60 ${selectedId === entry.id ? 'border-foreground bg-muted' : 'hover:bg-muted/50'}`}
              onClick={() => onSelect(entry)}
            >
              <CredentialProviderMark provider={entry.id} />
              <span className="min-w-0 break-words">
                <span className="block font-medium">{entry.name}</span>
                <span className="block text-xs text-muted-foreground">
                  {entry.category}
                </span>
                {entry.reason && (
                  <span className="mt-1 block text-xs">{entry.reason}</span>
                )}
              </span>
            </button>
          ))}
        </div>
      )}
      {catalog.data?.total === 0 && (
        <p className="text-sm text-muted-foreground">
          No services match this search. You can configure a custom HTTP service
          below.
        </p>
      )}
      {(catalog.data?.total ?? 0) > 20 && (
        <ResponsivePagination
          page={page}
          pageSize={20}
          total={catalog.data?.total ?? 0}
          totalPages={Math.ceil((catalog.data?.total ?? 0) / 20)}
          onPageChange={setPage}
        />
      )}
      <Button
        type="button"
        variant="outline"
        disabled={disabled}
        aria-pressed={selectedId === 'custom'}
        onClick={() =>
          onSelect({
            id: 'custom',
            name: 'Custom HTTP service',
            description: '',
            category: 'Custom',
            supported: true,
          })
        }
      >
        Custom HTTP service
      </Button>
    </section>
  )
}
