// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from './lib/cn'
import { fmtDateTime, fmtRelativeTime } from './fmt'

export interface CompactRowProps {
  /** ISO 8601 (or any `Date`-parseable) timestamp for the row's own event time. */
  timestamp: string
  icon: ReactNode
  primary: ReactNode
  secondary?: ReactNode
  meta?: ReactNode
  onClick?: () => void
  className?: string
}

/**
 * A one-line, tabular compact row: timestamp, icon, primary + secondary
 * text, trailing meta. Promoted from `ObserveRowShell`
 * (web/src/components/observe/rows/RowParts.tsx), which proved this shape
 * for the unified Observe feed — requests, spans, errors, revenue events —
 * generalized here for any dense event/log/activity list, not just Observe.
 * `RowParts.tsx` now re-exports this as `ObserveRowShell` and keeps its
 * Observe-specific `StatusBadge`/`SeverityBadge` (HTTP-status-code and
 * log-severity classifiers — different semantics from `Status`'s five-tone
 * health vocabulary, so they stay put rather than being forced onto it).
 */
export function CompactRow({
  timestamp,
  icon,
  primary,
  secondary,
  meta,
  onClick,
  className,
}: CompactRowProps) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'group flex w-full items-center gap-3 px-4 py-2 text-left',
        'border-b border-border/50 last:border-b-0',
        'hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none',
        'transition-colors',
        className,
      )}
    >
      <Timestamp value={timestamp} />
      <div className="flex h-5 w-5 shrink-0 items-center justify-center text-muted-foreground">
        {icon}
      </div>
      <div className="min-w-0 flex-1 truncate text-sm">
        <span className="font-medium">{primary}</span>
        {secondary != null && (
          <span className="ml-2 truncate text-muted-foreground">{secondary}</span>
        )}
      </div>
      {meta != null && (
        <div className="ml-auto flex shrink-0 items-center gap-2 text-xs text-muted-foreground">
          {meta}
        </div>
      )}
    </button>
  )
}

function Timestamp({ value }: { value: string }) {
  return (
    <time
      dateTime={value}
      title={fmtDateTime(value)}
      className="w-24 shrink-0 font-mono text-xs tabular-nums text-muted-foreground"
    >
      {fmtRelativeTime(value)}
    </time>
  )
}
