// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import ReactDOM from 'react-dom/client'
import { lazy, Suspense } from 'react'
import { ThemeProvider } from '@/components/providers/ThemeProvider'
import { InitialLoadScreen } from '@/components/layout/InitialLoadScreen'
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
          <InitialLoadScreen />
        </ThemeProvider>
      }
    >
      <Screen />
    </Suspense>
  )
}
