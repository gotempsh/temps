// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { emptyConsoleExtensions } from './extensions'
import {
  ConsoleExtensionsContext,
  type ConsoleExtensionsProviderProps,
} from './context-shared'

export function ConsoleExtensionsProvider({
  extensions,
  children,
}: ConsoleExtensionsProviderProps) {
  return (
    <ConsoleExtensionsContext.Provider
      value={extensions ?? emptyConsoleExtensions}
    >
      {children}
    </ConsoleExtensionsContext.Provider>
  )
}
