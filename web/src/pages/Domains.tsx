// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageContainer } from '@/components/layout/PageContainer'

import { useQuery } from '@tanstack/react-query'
import { listDomainsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { DomainSort } from '@/components/domains/domain-sort'
import { DomainsManagement } from '@/components/domains/DomainsManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useDebounce } from '@/hooks/useDebounce'
import { useEffect } from 'react'
import { useComposedSearchParams } from '@temps-sdk/ds'

const PAGE_SIZE = 20

export function Domains() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [params, updateParams] = useComposedSearchParams()
  const searchQuery = params.get('search') ?? ''
  const sort: DomainSort =
    params.get('sort') === 'domain' || params.get('sort') === 'status'
      ? (params.get('sort') as DomainSort)
      : 'expiration'
  const direction = params.get('direction') === 'desc' ? 'desc' : 'asc'
  const parsedPage = Number(params.get('page'))
  const page =
    Number.isSafeInteger(parsedPage) && parsedPage > 0 ? parsedPage : 1
  const debouncedSearch = useDebounce(searchQuery, 300)
  const query = {
    page,
    page_size: PAGE_SIZE,
    search: debouncedSearch || undefined,
    sort_by: sort,
    sort_order: direction,
  } as const
  const { data, isLoading, isError, refetch } = useQuery(
    listDomainsOptions({ query })
  )
  const patchParams = (values: Record<string, string>) =>
    updateParams((next) => {
      for (const [key, value] of Object.entries(values)) {
        if (value) next.set(key, value)
        else next.delete(key)
      }
      return next
    })
  const handleSearchChange = (value: string) =>
    patchParams({ search: value, page: '1' })
  const handleSortChange = (value: DomainSort) =>
    patchParams({
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

  const total = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE))

  useEffect(() => {
    if (data && page > totalPages) {
      updateParams((next) => {
        next.set('page', String(totalPages))
        return next
      })
    }
  }, [data, page, totalPages, updateParams])

  return (
    <PageContainer innerClassName="space-y-6">
      <div className="space-y-6">
        <DomainsManagement
          domains={data?.domains ?? []}
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
          onPageChange={(page) => patchParams({ page: String(page) })}
          searchQuery={searchQuery}
          onSearchChange={handleSearchChange}
          isSearching={searchQuery !== debouncedSearch}
        />
      </div>
    </PageContainer>
  )
}
