import { createContext, useContext, type ReactNode } from 'react'
import {
  type ConsoleExtensions,
  emptyConsoleExtensions,
} from './extensions'

const ConsoleExtensionsContext =
  createContext<ConsoleExtensions>(emptyConsoleExtensions)

export interface ConsoleExtensionsProviderProps {
  extensions?: ConsoleExtensions
  children: ReactNode
}

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

export function useConsoleExtensions(): ConsoleExtensions {
  return useContext(ConsoleExtensionsContext)
}
