// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { cn } from './lib/cn'

export interface ArticleProps {
  children: ReactNode
  className?: string
}

/**
 * Long-form prose: release notes, postmortems, docs pages — anything read
 * top to bottom rather than scanned for facts (that's `Detail`'s job). Built
 * on Tailwind Typography (already a `web` dependency, see `index.css`'s
 * `@plugin`), with color/link/code overrides so it stays inside the token
 * system instead of the plugin's default blue links and gray palette.
 */
export function Article({ children, className }: ArticleProps) {
  return (
    <div
      className={cn(
        'prose prose-neutral dark:prose-invert max-w-3xl',
        'prose-headings:tracking-tight prose-headings:font-semibold',
        'prose-a:text-foreground prose-a:underline prose-a:underline-offset-4',
        'prose-code:before:content-none prose-code:after:content-none',
        'prose-code:rounded-sm prose-code:bg-muted prose-code:px-1 prose-code:py-0.5 prose-code:font-mono prose-code:text-sm prose-code:font-normal',
        'prose-pre:rounded-md prose-pre:border prose-pre:bg-muted/30',
        'prose-blockquote:border-l-2 prose-blockquote:border-border prose-blockquote:font-normal prose-blockquote:not-italic',
        'prose-hr:border-border',
        className,
      )}
    >
      {children}
    </div>
  )
}
