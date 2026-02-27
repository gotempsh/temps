import { createContext, useContext, useMemo, type ReactNode } from 'react'
import { usePlugins } from '@/hooks/usePlugins'
import type { PluginManifest, NavEntry } from '@/types/plugins'

interface PluginsContextType {
  /** All loaded external plugin manifests */
  plugins: PluginManifest[]
  /** Whether the initial fetch is still loading */
  isLoading: boolean
  /** Nav entries for the platform sidebar section, sorted by order */
  platformNavEntries: NavEntry[]
  /** Nav entries for the settings sidebar section, sorted by order */
  settingsNavEntries: NavEntry[]
  /** Nav entries for the project detail sidebar, sorted by order */
  projectNavEntries: NavEntry[]
  /** Get a plugin manifest by name */
  getPlugin: (name: string) => PluginManifest | undefined
}

const PluginsContext = createContext<PluginsContextType | undefined>(undefined)

export function PluginsProvider({ children }: { children: ReactNode }) {
  const { data: plugins = [], isLoading } = usePlugins()

  const platformNavEntries = useMemo(
    () =>
      plugins
        .flatMap((p) => p.nav)
        .filter((e) => e.section === 'platform')
        .sort((a, b) => a.order - b.order),
    [plugins]
  )

  const settingsNavEntries = useMemo(
    () =>
      plugins
        .flatMap((p) => p.nav)
        .filter((e) => e.section === 'settings')
        .sort((a, b) => a.order - b.order),
    [plugins]
  )

  const projectNavEntries = useMemo(
    () =>
      plugins
        .flatMap((p) => p.nav)
        .filter((e) => e.section === 'project')
        .sort((a, b) => a.order - b.order),
    [plugins]
  )

  const getPlugin = (name: string) => plugins.find((p) => p.name === name)

  return (
    <PluginsContext.Provider
      value={{
        plugins,
        isLoading,
        platformNavEntries,
        settingsNavEntries,
        projectNavEntries,
        getPlugin,
      }}
    >
      {children}
    </PluginsContext.Provider>
  )
}

export function usePluginsContext() {
  const context = useContext(PluginsContext)
  if (context === undefined) {
    throw new Error('usePluginsContext must be used within a PluginsProvider')
  }
  return context
}
