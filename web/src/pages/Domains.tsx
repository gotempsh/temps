// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageContainer } from '@/components/layout/PageContainer'

import { useQuery } from '@tanstack/react-query'
import { listDomains } from '@/api/client/sdk.gen'
import type { DomainResponse } from '@/api/client/types.gen'
import { sortDomains, type DomainSort } from '@/components/domains/domain-sort'
import { useSearchParams } from 'react-router'
import { DomainsManagement } from '@/components/domains/DomainsManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useDebounce } from '@/hooks/useDebounce'
import { useEffect } from 'react'

const PAGE_SIZE = 20

export function Domains() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [params, setParams] = useSearchParams()
  const searchQuery = params.get('search') ?? ''
  const sort: DomainSort =
    params.get('sort') === 'domain' || params.get('sort') === 'status'
      ? (params.get('sort') as DomainSort)
      : 'expiration'
  const direction = params.get('direction') === 'desc' ? 'desc' : 'asc'
  const requestedPage = Math.max(1, Number(params.get('page')) || 1)
  const debouncedSearch = useDebounce(searchQuery, 300)
  const {
    data: domains = [],
    isLoading,
    isError,
    refetch,
  } = useQuery({
    queryKey: ['domains-table', debouncedSearch],
    queryFn: async ({ signal }) => {
      const rows = new Map<number, DomainResponse>()
      let page = 1
      let totalPages = 1
      do {
        const { data } = await listDomains({
          query: { page, page_size: 100, search: debouncedSearch || undefined },
          signal,
          throwOnError: true,
        })
        for (const domain of data.domains) rows.set(domain.id, domain)
        totalPages = Math.ceil(data.total / 100)
        page += 1
      } while (page <= totalPages)
      return [...rows.values()]
    },
  })
  const updateParams = (values: Record<string, string>) =>
    setParams(
      (previous) => {
        const next = new URLSearchParams(previous)
        for (const [key, value] of Object.entries(values)) {
          if (value) next.set(key, value)
          else next.delete(key)
        }
        return next
      },
      { replace: true }
    )
  const handleSearchChange = (value: string) =>
    updateParams({ search: value, page: '1' })
  const handleSortChange = (value: DomainSort) =>
    updateParams({
      sort: value,
      direction: sort === value && direction === 'asc' ? 'desc' : 'asc',
      page: '1',
    })

  useEffect(() => {
    setBreadcrumbs([{ label: 'Domains' }])
  }, [setBreadcrumbs])

  // Keyboard shortcut: N to add new domain
  useKeyboardShortcut({ key: 'n', path: '/domains/add' })

  usePageTitle('Domains')

  const total = domains.length
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))
  const page = Math.min(Math.floor(requestedPage), totalPages)
  const sorted = sortDomains(domains, sort, direction)

  return (
    <PageContainer innerClassName="space-y-6">
      <div className="space-y-6">
        <DomainsManagement
          domains={sorted.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE)}
          sort={sort}
          direction={direction}
          onSortChange={handleSortChange}
          isError={isError}
          isLoading={isLoading}
          reloadDomains={refetch}
          total={total}
          page={page}
          pageSize={PAGE_SIZE}
          totalPages={totalPages}
          onPageChange={(page) => updateParams({ page: String(page) })}
          searchQuery={searchQuery}
          onSearchChange={handleSearchChange}
          isSearching={searchQuery !== debouncedSearch}
        />
      </div>
    </PageContainer>
  )
}
