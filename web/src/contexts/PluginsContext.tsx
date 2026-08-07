import { createContext, useContext, useMemo, type ReactNode } from 'react'
import { usePlugins } from '@/hooks/usePlugins'
import type { PluginManifest, ResolvedNavEntry } from '@/types/plugins'

interface PluginsContextType {
  /**
   * All loaded external plugin manifests.
   *
   * For navigation use the `*NavEntries` below instead: a manifest's own
   * `nav[].path` is the plugin's internal route and does not match the
   * console's router.
   */
  plugins: PluginManifest[]
  /** Whether the initial fetch is still loading */
  isLoading: boolean
  /** Nav entries for the platform sidebar section, sorted by order */
  platformNavEntries: ResolvedNavEntry[]
  /** Nav entries for the settings sidebar section, sorted by order */
  settingsNavEntries: ResolvedNavEntry[]
  /** Nav entries for the project detail sidebar, sorted by order */
  projectNavEntries: ResolvedNavEntry[]
  /** Get a plugin manifest by name */
  getPlugin: (name: string) => PluginManifest | undefined
}

const PluginsContext = createContext<PluginsContextType | undefined>(undefined)

export function PluginsProvider({ children }: { children: ReactNode }) {
  const { data: plugins = [], isLoading } = usePlugins()

  // Build nav entries with resolved paths: /plugins/{pluginName} for
  // platform/settings sections so they match the <Route path="/plugins/:pluginName/*"> in App.tsx
  const resolvedEntries = useMemo<ResolvedNavEntry[]>(
    () =>
      plugins.flatMap((p) =>
        p.nav.map((entry) => ({
          ...entry,
          pluginName: p.name,
          path:
            entry.section === 'project'
              ? entry.path // Project entries stay relative
              : `/plugins/${p.name}`, // Platform/settings route through /plugins/:pluginName
        }))
      ),
    [plugins]
  )

  const platformNavEntries = useMemo(
    () =>
      resolvedEntries
        .filter((e) => e.section === 'platform')
        .sort((a, b) => a.order - b.order),
    [resolvedEntries]
  )

  const settingsNavEntries = useMemo(
    () =>
      resolvedEntries
        .filter((e) => e.section === 'settings')
        .sort((a, b) => a.order - b.order),
    [resolvedEntries]
  )

  const projectNavEntries = useMemo(
    () =>
      resolvedEntries
        .filter((e) => e.section === 'project')
        .sort((a, b) => a.order - b.order),
    [resolvedEntries]
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
