// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type KeyboardEvent, useId, useRef } from 'react'
import {
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
} from 'lucide-react'

import { Button } from './button'
import { Input } from '@temps-sdk/ui'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@temps-sdk/ui'
import { clampPage } from './lib/pagination'
import { cn } from './lib/cn'

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
  const pageInput = useRef<HTMLInputElement>(null)

  // Not a <form>: a pager often sits inside one (a picker on a settings
  // page), where a nested form's submit reaches the outer form and saves it.
  const goToRequestedPage = () => {
    const input = pageInput.current
    if (!input) return
    const nextPage = clampPage(Number(input.value), totalPages)
    input.value = input.defaultValue
    if (nextPage !== page) onPageChange(nextPage)
  }

  const goOnEnter = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key !== 'Enter') return
    // Enter in a text box would otherwise submit the surrounding form.
    event.preventDefault()
    goToRequestedPage()
  }

  const firstItem = total === 0 ? 0 : (page - 1) * pageSize + 1
  const lastItem = Math.min(page * pageSize, total)
  const canChangePageSize =
    onPageSizeChange !== undefined && pageSizeOptions.length > 0

  return (
    // By its own width: a pager in a narrow card has no more room than one on
    // a phone, whatever the window's size.
    <nav aria-label={ariaLabel} className={cn('@container w-full', className)}>
      <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-3 @xl:hidden">
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

      <div className="hidden flex-col gap-3 @xl:flex @2xl:flex-row @2xl:items-center @2xl:justify-between">
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
                  className="h-9 w-20"
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

          <div
            key={page}
            role="group"
            aria-label="Go to page"
            className="flex items-center gap-2"
          >
            <label htmlFor={pageInputId} className="text-sm">
              Page
            </label>
            <Input
              ref={pageInput}
              id={pageInputId}
              name="page"
              type="number"
              inputMode="numeric"
              min={1}
              max={totalPages}
              defaultValue={page}
              className="h-9 w-20"
              aria-label="Page number"
              onKeyDown={goOnEnter}
            />
            <span className="text-sm text-muted-foreground">
              of {totalPages}
            </span>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={goToRequestedPage}
            >
              Go
            </Button>
          </div>

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
