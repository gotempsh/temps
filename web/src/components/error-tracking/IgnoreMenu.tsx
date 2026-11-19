// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ChevronDown, EyeOff } from 'lucide-react'
import {
  IGNORE_PRESETS,
  ignorePresetConditions,
  type IgnoreConditions,
} from './ignore-presets'

interface IgnoreMenuProps {
  /** Called with the chosen conditions; `undefined` is a plain ignore. */
  onIgnore: (conditions: IgnoreConditions | undefined) => void
  disabled?: boolean
  /** Hide the label on narrow screens, as the bulk bar does. */
  compactLabel?: boolean
  size?: 'sm' | 'default'
}

/**
 * Ignore with a condition for coming back (ADR 074, D1.3): until the next
 * event, for a while, for more occurrences, until more users are affected,
 * or forever.
 */
export function IgnoreMenu({
  onIgnore,
  disabled,
  compactLabel,
  size = 'sm',
}: IgnoreMenuProps) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size={size} disabled={disabled}>
          <EyeOff className="h-3.5 w-3.5" />
          <span className={compactLabel ? 'max-sm:sr-only' : undefined}>
            Ignore
          </span>
          <ChevronDown className="h-3 w-3 opacity-60" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuLabel>Ignore, and bring it back</DropdownMenuLabel>
        <DropdownMenuSeparator />
        {IGNORE_PRESETS.map(({ preset, label }) => (
          <DropdownMenuItem
            key={preset}
            onClick={() => onIgnore(ignorePresetConditions(preset))}
          >
            {label}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
