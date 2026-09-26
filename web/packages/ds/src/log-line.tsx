// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { memo } from 'react'
import { cn } from './lib/cn'

export interface LogLineProps {
  content: string
  isHighlighted?: boolean
  searchTerm?: string
}

/**
 * One row of a monospace log stream: plain text with optional search-term
 * `<mark>` highlighting. Promoted from
 * `web/src/components/runtime-logs/log-line.tsx` (unchanged behavior — that
 * file now re-exports this) because it's small, self-contained and purely
 * presentational.
 *
 * Not currently wired into `log-viewer.tsx`/`history-log-viewer.tsx` — those
 * two files render ANSI-converted HTML via `dangerouslySetInnerHTML` with
 * their own inline (and already regex-escaped) highlight logic, which is a
 * genuinely different rendering path from this component's plain-text
 * children. They are intentionally NOT touched or repointed at `LogLine` in
 * this pass — see `design-system-handoff.md`'s follow-ups.
 */
export const LogLine = memo(function LogLine({
  content,
  isHighlighted,
  searchTerm,
}: LogLineProps) {
  const highlightSearchTerm = (text: string) => {
    if (!searchTerm) return text

    const parts = text.split(new RegExp(`(${searchTerm})`, 'gi'))
    return parts.map((part, i) =>
      part.toLowerCase() === searchTerm?.toLowerCase() ? (
        <mark key={i} className="rounded bg-warning/40 px-1 text-foreground">
          {part}
        </mark>
      ) : (
        part
      )
    )
  }

  return (
    <div
      className={cn(
        'py-0 px-2 whitespace-pre-wrap break-all font-mono text-xs leading-snug select-text',
        isHighlighted && 'bg-accent'
      )}
    >
      {highlightSearchTerm(content)}
    </div>
  )
})
