// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'
import type { PluginManifest, ResolvedNavEntry } from '@/types/plugins'

export interface PluginsContextType {
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

export const PluginsContext = createContext<PluginsContextType | undefined>(
  undefined
)

export function usePluginsContext() {
  const context = useContext(PluginsContext)
  if (context === undefined) {
    throw new Error('usePluginsContext must be used within a PluginsProvider')
  }
  return context
}
