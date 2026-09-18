// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useId, type ReactNode } from 'react'
import { CircleHelp, ChevronRight } from 'lucide-react'
import { Popover, PopoverContent, PopoverTrigger } from '@temps-sdk/ui'
import { Button } from './button'

/** Optional context only. Validation and consequences must stay visible. */
export function HelpPopover({
  label,
  children,
}: {
  label: string
  children: ReactNode
}) {
  const id = useId()
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-7 shrink-0 text-muted-foreground"
          aria-label={label}
        >
          <CircleHelp aria-hidden="true" className="size-4" />
        </Button>
      </PopoverTrigger>
      <PopoverContent
        align="start"
        aria-labelledby={id}
        className="max-w-[calc(100vw-2rem)] text-sm"
      >
        <p id={id} className="mb-2 font-medium">
          {label}
        </p>
        <div className="space-y-2 text-muted-foreground">{children}</div>
      </PopoverContent>
    </Popover>
  )
}

/** Native keyboard-accessible disclosure; label names the content it reveals. */
export function Disclosure({
  label,
  children,
}: {
  label: string
  children: ReactNode
}) {
  return (
    <details className="group text-sm">
      <summary className="flex w-fit cursor-pointer list-none items-center gap-1.5 rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        <ChevronRight
          aria-hidden="true"
          className="size-4 shrink-0 group-open:rotate-90"
        />
        {label}
      </summary>
      <div className="mt-3 space-y-3 text-muted-foreground">{children}</div>
    </details>
  )
}
