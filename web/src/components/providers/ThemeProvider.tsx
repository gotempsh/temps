// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

'use client'

import { ThemeProvider as NextThemesProvider } from 'next-themes'
import { type ThemeProviderProps } from 'next-themes'

export function ThemeProvider({ children, ...props }: ThemeProviderProps) {
  return (
    <NextThemesProvider
      {...props}
      // This console mounts entirely on the client. next-themes applies the
      // theme in its effect; its SSR bootstrap script cannot execute here.
      scriptProps={{ ...props.scriptProps, type: 'application/x-temps-theme' }}
    >
      {children}
    </NextThemesProvider>
  )
}

export { useTheme } from 'next-themes'
