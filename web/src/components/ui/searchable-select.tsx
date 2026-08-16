import * as React from 'react'
import { Check, ChevronsUpDown } from 'lucide-react'

import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from '@/components/ui/popover'

export interface SearchableSelectOption {
  value: string
  label: string
  /** Optional group label — items sharing a group render together under it. */
  group?: string
  /** Extra text included in the search string (e.g. description or id). */
  keywords?: string
  disabled?: boolean
}

interface SearchableSelectProps {
  value?: string
  onValueChange: (value: string) => void
  options: SearchableSelectOption[]
  placeholder?: string
  searchPlaceholder?: string
  emptyText?: string
  disabled?: boolean
  className?: string
  contentClassName?: string
  /** Use strict case-insensitive substring matching instead of cmdk fuzzy matching. */
  searchMode?: 'fuzzy' | 'contains'
}

export function SearchableSelect({
  value,
  onValueChange,
  options,
  placeholder = 'Select...',
  searchPlaceholder = 'Search...',
  emptyText = 'No results.',
  disabled,
  className,
  contentClassName,
  searchMode = 'fuzzy',
}: SearchableSelectProps) {
  const [open, setOpen] = React.useState(false)

  const selected = React.useMemo(
    () => options.find((o) => o.value === value),
    [options, value]
  )

  const grouped = React.useMemo(() => {
    const groups = new Map<string, SearchableSelectOption[]>()
    for (const opt of options) {
      const key = opt.group ?? ''
      const list = groups.get(key) ?? []
      list.push(opt)
      groups.set(key, list)
    }
    return Array.from(groups.entries())
  }, [options])

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="outline"
          role="combobox"
          aria-expanded={open}
          disabled={disabled}
          className={cn(
            'h-10 w-full justify-between font-normal',
            !selected && 'text-muted-foreground',
            className
          )}
        >
          <span className="truncate">{selected?.label ?? placeholder}</span>
          <ChevronsUpDown className="ml-2 h-4 w-4 shrink-0 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        className={cn(
          'w-[min(calc(100vw-2rem),360px)] min-w-[var(--radix-popover-trigger-width)] p-0',
          contentClassName
        )}
        align="start"
      >
        <Command
          filter={
            searchMode === 'contains'
              ? (itemValue, search) =>
                  itemValue.toLowerCase().includes(search.trim().toLowerCase())
                    ? 1
                    : 0
              : undefined
          }
        >
          <CommandInput placeholder={searchPlaceholder} />
          <CommandList className="max-h-[320px]">
            <CommandEmpty>{emptyText}</CommandEmpty>
            {grouped.map(([group, items]) => (
              <CommandGroup
                key={group || '__default'}
                heading={group || undefined}
              >
                {items.map((opt) => (
                  <CommandItem
                    key={opt.value}
                    value={`${opt.label} ${opt.keywords ?? ''} ${opt.value}`}
                    disabled={opt.disabled}
                    onSelect={() => {
                      onValueChange(opt.value)
                      setOpen(false)
                    }}
                  >
                    <Check
                      className={cn(
                        'mr-2 h-4 w-4 shrink-0',
                        value === opt.value ? 'opacity-100' : 'opacity-0'
                      )}
                    />
                    <span className="truncate">{opt.label}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  )
}
