// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { Search, X } from 'lucide-react'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'

/** Legacy console list controls: search first, view context second. */
export function ListToolbar({
  value,
  onChange,
  searchLabel,
  placeholder,
  summary,
  children,
}: {
  value: string
  onChange: (value: string) => void
  searchLabel: string
  placeholder: string
  summary?: ReactNode
  children?: ReactNode
}) {
  return (
    <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-center sm:justify-between">
      <div className="relative w-full sm:w-80">
        <Search
          aria-hidden="true"
          className="pointer-events-none absolute start-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
        />
        <Input
          aria-label={searchLabel}
          placeholder={placeholder}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Escape') onChange('')
          }}
          className="ps-9 pe-10"
        />
        {value && (
          <Button
            type="button"
            variant="ghost"
            size="icon"
            aria-label={`Clear ${searchLabel.toLowerCase()}`}
            className="absolute end-1 top-1/2 size-7 -translate-y-1/2"
            onClick={() => onChange('')}
          >
            <X aria-hidden="true" className="size-4" />
          </Button>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-3">
        {summary && (
          <p className="text-sm text-muted-foreground" role="status">
            {summary}
          </p>
        )}
        {children}
      </div>
    </div>
  )
}
