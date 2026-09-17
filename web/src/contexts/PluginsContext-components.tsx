// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useMemo, type ReactNode } from 'react'
import { usePlugins } from '@/hooks/usePlugins'
import type { ResolvedNavEntry } from '@/types/plugins'
import { PluginsContext } from './PluginsContext-shared'

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
