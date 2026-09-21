// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { Link, type LinkProps } from 'react-router'
import { ArrowRight } from 'lucide-react'
import { cn } from './lib/cn'

/** Canonical resource-detail navigation in a table's identity column. */
export function RecordLink({ children, className, ...props }: LinkProps) {
  return (
    <Link
      {...props}
      className={cn(
        'inline-flex min-h-9 max-w-full items-center gap-2 rounded-sm text-sm font-medium text-foreground underline decoration-muted-foreground underline-offset-4 hover:decoration-foreground focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-4',
        className
      )}
    >
      <span className="min-w-0 break-all">{children}</span>
      <ArrowRight className="size-4 shrink-0" aria-hidden="true" />
    </Link>
  )
}
