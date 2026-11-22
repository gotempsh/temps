// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Columns3 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import type { TableColumnOption } from './useTableColumns'

export function TableColumnPicker<Key extends string>({
  options,
  visibleKeys,
  onChange,
  onReset,
}: {
  options: readonly TableColumnOption<Key>[]
  visibleKeys: readonly Key[]
  onChange: (keys: Key[]) => void
  onReset: () => void
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="sm">
          <Columns3 className="mr-2 size-4" aria-hidden="true" />
          Columns
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>Visible columns</DropdownMenuLabel>
        {options.map((column) => (
          <DropdownMenuCheckboxItem
            key={column.key}
            checked={visibleKeys.includes(column.key)}
            disabled={column.required}
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(checked) =>
              onChange(
                checked
                  ? [...visibleKeys, column.key]
                  : visibleKeys.filter((key) => key !== column.key)
              )
            }
          >
            {column.label}
            {column.required && (
              <span className="ml-2 text-xs text-muted-foreground">
                Required
              </span>
            )}
          </DropdownMenuCheckboxItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={onReset}>
          Reset to defaults
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
