// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ResponsivePagination } from '@/components/ui/responsive-pagination'

export const SERVICE_CATALOG_PAGE_SIZE = 24

export interface ServiceCatalogPaginationProps {
  page: number
  total: number
  totalPages: number
  onPageChange: (page: number) => void
}

export function ServiceCatalogPagination({
  page,
  total,
  totalPages,
  onPageChange,
}: ServiceCatalogPaginationProps) {
  return (
    <ResponsivePagination
      page={page}
      pageSize={SERVICE_CATALOG_PAGE_SIZE}
      total={total}
      totalPages={Math.max(1, totalPages)}
      ariaLabel="Service catalog pagination"
      className="shrink-0"
      onPageChange={onPageChange}
    />
  )
}
