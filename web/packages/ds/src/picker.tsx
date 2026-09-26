// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ComponentProps, ReactNode } from 'react'
import { Check } from 'lucide-react'
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@temps-sdk/ui'
import { cn } from './lib/cn'

export interface PickerItem<T extends string = string> {
  value: T
  label: ReactNode
  keywords?: string[]
  icon?: ReactNode
  description?: ReactNode
  disabled?: boolean
}

export interface PickerProps<T extends string = string> {
  items: PickerItem<T>[]
  value?: T
  onValueChange: (value: T) => void
  placeholder?: string
  emptyMessage?: ReactNode
  className?: string
  /** Props from Field belong on the search input, not the surrounding list. */
  inputProps?: Pick<ComponentProps<typeof CommandInput>, 'id' | 'aria-label' | 'aria-labelledby' | 'aria-describedby' | 'aria-invalid'>
}

/** An inline searchable list-and-select control, for pickers embedded in a page (not a modal). */
export function Picker<T extends string = string>({
  items,
  value,
  onValueChange,
  placeholder = 'Search…',
  emptyMessage = 'No matches.',
  className,
  inputProps,
}: PickerProps<T>) {
  return (
    <Command label="Search options" className={cn('rounded-md border', className)}>
      <CommandInput asChild placeholder={placeholder} className="aria-invalid:text-destructive">
        <input {...inputProps} />
      </CommandInput>
      <CommandList>
        <CommandEmpty>{emptyMessage}</CommandEmpty>
        <CommandGroup>
          {items.map((item) => (
            <CommandItem
              key={item.value}
              value={item.value}
              keywords={[...(typeof item.label === 'string' ? [item.label] : []), ...(item.keywords ?? [])]}
              disabled={item.disabled}
              onSelect={() => onValueChange(item.value)}
            >
              {item.icon ? <span className="shrink-0" aria-hidden>{item.icon}</span> : null}
              <div className="flex min-w-0 flex-col">
                <span className="truncate">{item.label}</span>
                {item.description ? (
                  <span className="truncate text-xs text-muted-foreground">
                    {item.description}
                  </span>
                ) : null}
              </div>
              {value === item.value ? <span className="ml-auto shrink-0"><Check className="size-4" aria-hidden /><span className="sr-only">Selected</span></span> : null}
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </Command>
  )
}
