// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext, type ReactNode } from 'react'
import { type ConsoleExtensions, emptyConsoleExtensions } from './extensions'

export const ConsoleExtensionsContext = createContext<ConsoleExtensions>(
  emptyConsoleExtensions
)

export interface ConsoleExtensionsProviderProps {
  extensions?: ConsoleExtensions
  children: ReactNode
}

export function useConsoleExtensions(): ConsoleExtensions {
  return useContext(ConsoleExtensionsContext)
}
