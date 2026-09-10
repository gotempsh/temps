// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { listDomainsOptions } from '@/api/client/@tanstack/react-query.gen'
import { DomainsManagement } from '@/components/domains/DomainsManagement'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useKeyboardShortcut } from '@/hooks/useKeyboardShortcut'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useDebounce } from '@/hooks/useDebounce'
import { useEffect, useState } from 'react'

const PAGE_SIZE = 20

export function Domains() {
  const { setBreadcrumbs } = useBreadcrumbs()
  const [page, setPage] = useState(1)
  const [searchQuery, setSearchQuery] = useState('')
  const debouncedSearch = useDebounce(searchQuery, 300)

  const {
    data: domainsData,
    isLoading,
    refetch,
  } = useQuery({
    ...listDomainsOptions({
      query: {
        page,
        page_size: PAGE_SIZE,
        search: debouncedSearch || undefined,
      },
    }),
  })

  const handleSearchChange = (value: string) => {
    setSearchQuery(value)
    setPage(1)
  }

  useEffect(() => {
    setBreadcrumbs([{ label: 'Domains' }])
  }, [setBreadcrumbs])

  // Keyboard shortcut: N to add new domain
  useKeyboardShortcut({ key: 'n', path: '/domains/add' })

  usePageTitle('Domains')

  const total = domainsData?.total ?? 0
  const totalPages = Math.ceil(total / PAGE_SIZE)

  return (
    <div className="flex-1 overflow-auto">
      <div className="space-y-6">
        <DomainsManagement
          domains={domainsData?.domains || []}
          isLoading={isLoading}
          reloadDomains={refetch}
          total={total}
          page={page}
          pageSize={PAGE_SIZE}
          totalPages={totalPages}
          onPageChange={setPage}
          searchQuery={searchQuery}
          onSearchChange={handleSearchChange}
          isSearching={searchQuery !== debouncedSearch}
        />
      </div>
    </div>
  )
}
