// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { RefreshCw, Settings as SettingsIcon } from 'lucide-react'
import { Button } from './ui/button'
import { Skeleton } from './ui/skeleton'
import { cn } from './lib/cn'

/**
 * One component for every non-happy state of a surface. The console today
 * has three empty-state implementations, 134 files with spinners and 141
 * with skeletons; this replaces all of them.
 *
 *  loading       skeleton rows. Never a spinner as page state.
 *  empty         nothing to show and that is fine. Says why, offers a next step.
 *  unconfigured  depends on operator setup that is missing. Says exactly what
 *                is missing, shows an EXAMPLE of what the surface would show,
 *                links to the settings page. Never renders nothing.
 *  error         the surface failed. Message, the resource involved, retry.
 *
 * Self-hosted users have no support channel. A failure that needs a restart
 * to notice is a design failure.
 */
export type PageStateProps =
  | { state: 'loading'; rows?: number }
  | { state: 'empty'; title: string; reason: string; next?: ReactNode }
  | { state: 'unconfigured'; title: string; missing: string; example: ReactNode; /** A real console path (`/settings/…`). `#` is not allowed: the link to the fix is the point of this state. */ settingsHref: `/${string}`; settingsLabel: string }
  | { state: 'error'; title: string; message: string; resource: string; onRetry: () => void; retrying?: boolean }

export function PageState(p: PageStateProps & { className?: string }) {
  if (p.state === 'loading') {
    return (
      <div className={cn('op-rows border', p.className)} role="status" aria-busy="true" aria-label="Loading">
        {Array.from({ length: p.rows ?? 5 }, (_, i) => (
          <div key={i} className="op-row flex items-center gap-3">
            {/* Percent widths: fixed px widths give the row a min-content wider than a narrow grid cell and blow the layout. */}
            <Skeleton className="h-3 w-[28%] rounded-none" />
            <Skeleton className="h-3 w-[40%] rounded-none" />
            <Skeleton className="ml-auto h-3 w-[12%] rounded-none" />
          </div>
        ))}
      </div>
    )
  }
  if (p.state === 'empty') {
    return (
      <div className={cn('border p-6', p.className)}>
        <p className="op-h3">{p.title}</p>
        <p className="op-prose mt-1 max-w-md text-xs text-muted-foreground">{p.reason}</p>
        {p.next && <div className="mt-4">{p.next}</div>}
      </div>
    )
  }
  if (p.state === 'unconfigured') {
    // Both columns are min-w-0: a grid item's automatic minimum is its min-content width, so an unbreakable token in `example` (a curl body, a long URL) would otherwise widen the page on a phone.
    return (
      <div className={cn('grid min-w-0 border md:grid-cols-2 [&>*]:min-w-0', p.className)}>
        <div className="p-6">
          <p className="op-label">not set up</p>
          <p className="op-h3 mt-2">{p.title}</p>
          <p className="op-prose mt-1 max-w-md text-xs text-muted-foreground">Missing: {p.missing}</p>
          <Button size="sm" className="op-primary mt-4 h-8 text-xs" asChild>
            <a href={p.settingsHref}><SettingsIcon /> {p.settingsLabel}</a>
          </Button>
        </div>
        <div className="op-inset border-t p-4 md:border-l md:border-t-0">
          <p className="op-label">what this shows once configured</p>
          <div className="mt-3 opacity-80">{p.example}</div>
        </div>
      </div>
    )
  }
  return (
    <div className={cn('border border-destructive p-6', p.className)} role="alert">
      <p className="op-h3 text-destructive">{p.title}</p>
      <p className="mt-1 font-mono text-xs">{p.message}</p>
      <p className="mt-1 text-xs text-muted-foreground">resource: <span className="font-mono">{p.resource}</span></p>
      <Button size="sm" variant="outline" className="mt-4 h-8 text-xs" onClick={p.onRetry} disabled={p.retrying}>
        <RefreshCw className={cn(p.retrying && 'animate-spin')} /> {p.retrying ? 'retrying…' : 'retry'}
      </Button>
    </div>
  )
}
