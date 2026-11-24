// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Skeleton } from '@/components/ui/skeleton'
import { useConsoleExtensions } from '@temps-sdk/console-kit'

const NAV_ROWS = [0, 1, 2, 3, 4, 5, 6]
const CONTENT_ROWS = [0, 1, 2]

/**
 * What a page looks like before its data or code has arrived: a title, a
 * toolbar and a few cards. Sized like the real pages so nothing jumps when
 * they replace it.
 */
export function PageSkeleton() {
  return (
    <div
      role="status"
      aria-label="Loading page"
      className="space-y-6 px-4 py-2 sm:px-0"
    >
      <div className="flex items-center justify-between gap-4">
        <div className="space-y-2">
          <Skeleton className="h-7 w-48" />
          <Skeleton className="h-4 w-72 max-w-full" />
        </div>
        <Skeleton className="h-10 w-36" />
      </div>
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 lg:grid-cols-3">
        {CONTENT_ROWS.map((row) => (
          <Skeleton key={row} className="h-44 w-full rounded-xl" />
        ))}
      </div>
    </div>
  )
}

/**
 * The console's first screen while it checks who you are: the real shell's
 * outline (sidebar with the product mark, header, page) instead of a bare
 * spinner, so the layout is already in place when the data arrives.
 */
export function AppLoadingScreen() {
  const { logoIcon, logoText } = useConsoleExtensions()
  return (
    <div
      role="status"
      aria-label="Loading Temps"
      className="flex h-dvh w-full bg-background"
    >
      <aside className="hidden w-64 shrink-0 flex-col gap-6 border-r p-3 md:flex">
        <div className="flex items-center gap-2 px-1 pt-1">
          <span className="flex size-8 items-center justify-center">
            {logoIcon ?? (
              <img src="/svg/temps-icon.svg" alt="" className="size-full" />
            )}
          </span>
          <span className="text-sm font-semibold">{logoText ?? 'Temps'}</span>
        </div>
        <div className="space-y-2">
          {NAV_ROWS.map((row) => (
            <Skeleton key={row} className="h-8 w-full" />
          ))}
        </div>
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 shrink-0 items-center gap-3 border-b px-4">
          <span className="flex size-7 items-center justify-center md:hidden">
            {logoIcon ?? (
              <img src="/svg/temps-icon.svg" alt="" className="size-full" />
            )}
          </span>
          <Skeleton className="h-4 w-32" />
        </header>
        <main className="min-h-0 flex-1 overflow-hidden p-4">
          <PageSkeleton />
        </main>
      </div>
    </div>
  )
}
