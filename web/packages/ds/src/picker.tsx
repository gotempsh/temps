// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
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
}

export interface PickerProps<T extends string = string> {
  items: PickerItem<T>[]
  value?: T
  onValueChange: (value: T) => void
  placeholder?: string
  emptyMessage?: ReactNode
  className?: string
}

/** An inline searchable list-and-select control, for pickers embedded in a page (not a modal). */
export function Picker<T extends string = string>({
  items,
  value,
  onValueChange,
  placeholder = 'Search…',
  emptyMessage = 'No matches.',
  className,
}: PickerProps<T>) {
  return (
    <Command className={cn('rounded-md border', className)}>
      <CommandInput placeholder={placeholder} />
      <CommandList>
        <CommandEmpty>{emptyMessage}</CommandEmpty>
        <CommandGroup>
          {items.map((item) => (
            <CommandItem
              key={item.value}
              value={[item.value, ...(item.keywords ?? [])].join(' ')}
              onSelect={() => onValueChange(item.value)}
            >
              {item.icon}
              <div className="flex min-w-0 flex-col">
                <span className="truncate">{item.label}</span>
                {item.description ? (
                  <span className="truncate text-xs text-muted-foreground">
                    {item.description}
                  </span>
                ) : null}
              </div>
              {value === item.value ? <Check className="ml-auto size-4" /> : null}
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </Command>
  )
}
