// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import * as React from 'react'
import { Check, ChevronsUpDown, X } from 'lucide-react'

import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'

export interface FacetOption {
  /** Raw value sent to the API (e.g. "United States"). */
  value: string
  /** Distinct visitor count for this value in the current segment. */
  count: number
  /** Optional secondary code (e.g. 2-letter country code for flags). */
  code?: string | null
}

interface FacetComboboxProps {
  label: string
  value: string | undefined
  options: FacetOption[]
  onChange: (next: string | undefined) => void
  /** Show a flag emoji derived from `option.code` (country code). */
  withFlag?: boolean
  loading?: boolean
  placeholder?: string
  /** Optional explicit ID/label for the trigger (for a11y). */
  id?: string
  className?: string
}

function codeToFlag(code?: string | null): string {
  if (!code || code.length !== 2) return ''
  const codePoints = code
    .toUpperCase()
    .split('')
    .map((c) => 127397 + c.charCodeAt(0))
  return String.fromCodePoint(...codePoints)
}

function formatCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 10_000) return `${Math.round(n / 1_000)}k`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return n.toLocaleString()
}

/**
 * Dropdown filter populated from real data with visitor counts.
 *
 *   ┌─ Country ──────────────────────────┐
 *   │ 🇺🇸  United States      12.3k       │
 *   │ 🇩🇪  Germany             4.2k       │
 *   │ 🇫🇷  France                890       │
 *   └────────────────────────────────────┘
 *
 * Designed for use inside an inline filter row — full-width on mobile,
 * fixed-width on desktop. Clear (X) button appears when a value is set.
 */
export function FacetCombobox({
  label,
  value,
  options,
  onChange,
  withFlag,
  loading,
  placeholder,
  id,
  className,
}: FacetComboboxProps) {
  const [open, setOpen] = React.useState(false)

  const selected = React.useMemo(
    () => options.find((o) => o.value === value),
    [options, value]
  )

  // If a value is selected but not in the options list (e.g. it dropped out
  // of the top N), still render the chip so the user can clear it.
  const showSelectedNotInList = value && !selected
  const selectedFlag = withFlag ? codeToFlag(selected?.code) : ''

  const handleClear = (e: React.MouseEvent) => {
    e.stopPropagation()
    e.preventDefault()
    onChange(undefined)
  }

  return (
    <div className={cn('flex items-center gap-1.5', className)}>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            id={id}
            type="button"
            variant="outline"
            role="combobox"
            aria-expanded={open}
            aria-label={`${label} filter`}
            className={cn(
              'h-9 w-full justify-between gap-1.5 px-3 font-normal',
              !value && 'text-muted-foreground',
            )}
          >
            <span className="flex items-center gap-1.5 min-w-0">
              <span className="truncate text-xs uppercase tracking-wide text-muted-foreground">
                {label}
              </span>
              {value ? (
                <>
                  <span className="text-muted-foreground/40">·</span>
                  {selectedFlag && (
                    <span className="text-base leading-none">{selectedFlag}</span>
                  )}
                  <span className="truncate text-foreground">
                    {value}
                  </span>
                </>
              ) : (
                <>
                  <span className="text-muted-foreground/40">·</span>
                  <span className="truncate">
                    {placeholder ?? 'Any'}
                  </span>
                </>
              )}
            </span>
            <span className="flex items-center gap-1 shrink-0">
              {value && (
                <span
                  role="button"
                  tabIndex={0}
                  aria-label={`Clear ${label} filter`}
                  onClick={handleClear}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' || e.key === ' ') {
                      e.preventDefault()
                      onChange(undefined)
                    }
                  }}
                  className="inline-flex h-5 w-5 items-center justify-center rounded hover:bg-muted text-muted-foreground hover:text-foreground"
                >
                  <X className="h-3 w-3" />
                </span>
              )}
              <ChevronsUpDown className="h-3.5 w-3.5 opacity-50" />
            </span>
          </Button>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="w-[min(calc(100vw-2rem),320px)] min-w-[var(--radix-popover-trigger-width)] p-0"
        >
          <Command>
            <CommandInput placeholder={`Search ${label.toLowerCase()}...`} />
            <CommandList className="max-h-[280px]">
              {loading ? (
                <div className="py-6 text-center text-sm text-muted-foreground">
                  Loading...
                </div>
              ) : options.length === 0 ? (
                <CommandEmpty>No values in this range.</CommandEmpty>
              ) : (
                <>
                  <CommandEmpty>No matches.</CommandEmpty>
                  {showSelectedNotInList && value && (
                    <CommandItem
                      key={`__active__${value}`}
                      value={value}
                      onSelect={() => {
                        onChange(undefined)
                        setOpen(false)
                      }}
                      className="gap-2"
                    >
                      <Check className="h-4 w-4 shrink-0 opacity-100" />
                      <span className="truncate">{value}</span>
                      <span className="ml-auto text-[11px] text-muted-foreground">
                        selected
                      </span>
                    </CommandItem>
                  )}
                  {options.map((opt) => {
                    const flag = withFlag ? codeToFlag(opt.code) : ''
                    const isSelected = opt.value === value
                    return (
                      <CommandItem
                        key={opt.value}
                        value={opt.value}
                        onSelect={() => {
                          onChange(isSelected ? undefined : opt.value)
                          setOpen(false)
                        }}
                        className="gap-2"
                      >
                        <Check
                          className={cn(
                            'h-4 w-4 shrink-0',
                            isSelected ? 'opacity-100' : 'opacity-0'
                          )}
                        />
                        {flag && (
                          <span className="text-base leading-none shrink-0">
                            {flag}
                          </span>
                        )}
                        <span className="truncate">{opt.value}</span>
                        <span
                          className="ml-auto text-[11px] tabular-nums text-muted-foreground"
                          title={`${opt.count.toLocaleString()} visitors`}
                        >
                          {formatCount(opt.count)}
                        </span>
                      </CommandItem>
                    )
                  })}
                </>
              )}
            </CommandList>
          </Command>
        </PopoverContent>
      </Popover>
    </div>
  )
}
