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
import { CheckCircle2, ChevronDown, PackageCheck } from 'lucide-react'

interface ResolveMenuProps {
  /** `inNextRelease` keeps the group resolved until a newer release reports it. */
  onResolve: (inNextRelease: boolean) => void
  disabled?: boolean
  /** Hide the label on narrow screens, as the bulk bar does. */
  compactLabel?: boolean
}

/**
 * Resolve now, or resolve in the next release (ADR 111, D1.5) for a fix that
 * is merged but not deployed: events from the release running today keep the
 * group resolved, and the first event from a newer release reopens it.
 */
export function ResolveMenu({
  onResolve,
  disabled,
  compactLabel,
}: ResolveMenuProps) {
  return (
    <div className="flex items-center">
      <Button
        size="sm"
        variant="outline"
        className="rounded-r-none"
        disabled={disabled}
        onClick={() => onResolve(false)}
      >
        <CheckCircle2 className="h-3.5 w-3.5" />
        <span className={compactLabel ? 'max-sm:sr-only' : undefined}>
          Resolve
        </span>
      </Button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            size="sm"
            variant="outline"
            className="-ml-px rounded-l-none px-1.5"
            disabled={disabled}
            aria-label="More ways to close the group"
          >
            <ChevronDown className="h-3 w-3 opacity-60" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="max-w-72">
          <DropdownMenuLabel>Close the group</DropdownMenuLabel>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => onResolve(false)}>
            <CheckCircle2 className="mr-2 h-4 w-4" />
            Resolve now
          </DropdownMenuItem>
          <DropdownMenuItem
            onClick={() => onResolve(true)}
            className="items-start"
          >
            <PackageCheck className="mr-2 mt-0.5 h-4 w-4 shrink-0" />
            <span className="flex flex-col">
              <span>Resolve in next release</span>
              <span className="text-xs text-muted-foreground">
                Stays resolved for the release running now; a newer release
                reopens it
              </span>
            </span>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}
