// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import ReactDOM from 'react-dom/client'
import { lazy, Suspense } from 'react'
import { ThemeProvider } from '@/components/providers/ThemeProvider'
import { Skeleton } from '@/components/ui/skeleton'
import './globals.css'

// The entry point mounts the root; it is not a reusable component module.
// eslint-disable-next-line react-refresh/only-export-components
const Screen = lazy(() =>
  import('./App').then((m) => ({ default: m.TempsConsole }))
)

const rootEl = document.getElementById('root')
if (rootEl) {
  const root = ReactDOM.createRoot(rootEl)
  root.render(
    <Suspense
      fallback={
        <ThemeProvider defaultTheme="system" enableSystem attribute="class">
          <div
            role="status"
            aria-label="Loading application"
            className="flex min-h-dvh items-center justify-center bg-background text-foreground"
          >
            <div className="flex flex-col items-center gap-6">
              <div className="flex items-center gap-2">
                <img src="/favicon.png" alt="" className="size-8" />
                <span className="text-xl font-semibold tracking-tight">
                  Temps
                </span>
              </div>
              <Skeleton className="h-1 w-32 motion-reduce:animate-none" />
            </div>
          </div>
        </ThemeProvider>
      }
    >
      <Screen />
    </Suspense>
  )
}
