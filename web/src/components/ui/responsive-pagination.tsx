// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type FormEvent, useId } from 'react'
import {
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
} from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { clampPage } from '@/lib/pagination'
import { cn } from '@/lib/utils'

export interface ResponsivePaginationProps {
  page: number
  pageSize: number
  total: number
  totalPages: number
  onPageChange: (page: number) => void
  onPageSizeChange?: (pageSize: number) => void
  pageSizeOptions?: readonly number[]
  ariaLabel?: string
  pageSizeAriaLabel?: string
  className?: string
}

export function ResponsivePagination({
  page,
  pageSize,
  total,
  totalPages,
  onPageChange,
  onPageSizeChange,
  pageSizeOptions = [],
  ariaLabel = 'Pagination',
  pageSizeAriaLabel = 'Items per page',
  className,
}: ResponsivePaginationProps) {
  const pageInputId = useId()

  const submitRequestedPage = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const formData = new FormData(event.currentTarget)
    const parsed = Number(formData.get('page'))
    const nextPage = clampPage(parsed, totalPages)
    event.currentTarget.reset()
    if (nextPage !== page) onPageChange(nextPage)
  }

  const firstItem = (page - 1) * pageSize + 1
  const lastItem = Math.min(page * pageSize, total)
  const canChangePageSize =
    onPageSizeChange !== undefined && pageSizeOptions.length > 0

  return (
    <nav aria-label={ariaLabel} className={cn(className)}>
      <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 sm:hidden">
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="justify-self-start"
          onClick={() => onPageChange(page - 1)}
          disabled={page <= 1}
          aria-label="Previous page"
        >
          <ChevronLeft />
          Previous
        </Button>
        <span
          className="text-sm tabular-nums text-muted-foreground"
          aria-label={`Page ${page} of ${totalPages}`}
          aria-current="page"
        >
          {page} / {totalPages}
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="justify-self-end"
          onClick={() => onPageChange(page + 1)}
          disabled={page >= totalPages}
          aria-label="Next page"
        >
          Next
          <ChevronRight />
        </Button>
      </div>

      <div className="hidden flex-col gap-3 sm:flex lg:flex-row lg:items-center lg:justify-between">
        <div className="flex flex-wrap items-center gap-3 text-sm text-muted-foreground">
          <span>
            Showing {firstItem}–{lastItem} of {total}
          </span>
          {canChangePageSize && (
            <div className="flex items-center gap-2">
              <span>Per page</span>
              <Select
                value={String(pageSize)}
                onValueChange={(value) => onPageSizeChange(Number(value))}
              >
                <SelectTrigger
                  className="h-9 w-[76px]"
                  aria-label={pageSizeAriaLabel}
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {pageSizeOptions.map((size) => (
                    <SelectItem key={size} value={String(size)}>
                      {size}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onPageChange(1)}
            disabled={page <= 1}
            aria-label="Go to first page"
            title="First page"
          >
            <ChevronsLeft />
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onPageChange(page - 1)}
            disabled={page <= 1}
            aria-label="Go to previous page"
            title="Previous page"
          >
            <ChevronLeft />
          </Button>

          <form
            key={page}
            className="flex items-center gap-2"
            onSubmit={submitRequestedPage}
          >
            <label htmlFor={pageInputId} className="text-sm">
              Page
            </label>
            <Input
              id={pageInputId}
              name="page"
              type="number"
              inputMode="numeric"
              min={1}
              max={totalPages}
              defaultValue={page}
              className="h-9 w-20"
              aria-label="Page number"
            />
            <span className="text-sm text-muted-foreground">
              of {totalPages}
            </span>
            <Button type="submit" variant="outline" size="sm">
              Go
            </Button>
          </form>

          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onPageChange(page + 1)}
            disabled={page >= totalPages}
            aria-label="Go to next page"
            title="Next page"
          >
            <ChevronRight />
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onPageChange(totalPages)}
            disabled={page >= totalPages}
            aria-label="Go to last page"
            title="Last page"
          >
            <ChevronsRight />
          </Button>
        </div>
      </div>
    </nav>
  )
}
