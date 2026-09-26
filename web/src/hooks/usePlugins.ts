// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  getPluginInstallationReporting,
  listExternalPlugins,
  listPluginCatalog,
  reloadPlugins,
  setPluginInstallationReporting,
  uninstallPlugin,
} from '@/api/client/sdk.gen'
import type {
  PluginCatalogResponse,
  ReloadResponse,
} from '@/api/client/types.gen'
import type { PluginManifest } from '@/types/plugins'
import {
  queryOptions,
  useMutation,
  useQuery,
  useQueryClient,
} from '@tanstack/react-query'

export const PLUGINS_QUERY_KEY = ['external-plugins']
export const PLUGIN_CATALOG_QUERY_KEY = ['external-plugins', 'catalog']
export const PLUGIN_REPORTING_QUERY_KEY = [
  'external-plugins',
  'installation-reporting',
]

export function usePluginInstallationReporting(enabled = true) {
  return useQuery({
    queryKey: PLUGIN_REPORTING_QUERY_KEY,
    queryFn: async () =>
      (await getPluginInstallationReporting({ throwOnError: true })).data,
    enabled,
    retry: false,
  })
}

export function useSetPluginInstallationReporting() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (enabled: boolean) =>
      (
        await setPluginInstallationReporting({
          body: { enabled },
          throwOnError: true,
        })
      ).data,
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: PLUGIN_REPORTING_QUERY_KEY,
      })
    },
  })
}

/**
 * Fetch the list of running external plugin manifests.
 * The endpoint is optional, so startup without a plugin host degrades to an
 * empty list instead of breaking navigation throughout the dashboard.
 */
async function fetchPluginManifests(): Promise<PluginManifest[]> {
  try {
    const response = await listExternalPlugins({ throwOnError: true })
    return (response.data ?? []) as PluginManifest[]
  } catch {
    return []
  }
}

/**
 * React Query hook to get the list of running external plugins.
 * Caches for 5 minutes since plugins rarely change at runtime.
 */
export function pluginManifestQueryOptions() {
  return queryOptions({
    queryKey: PLUGINS_QUERY_KEY,
    queryFn: fetchPluginManifests,
    staleTime: 5 * 60 * 1000,
    gcTime: 10 * 60 * 1000,
    // CLI installs happen outside this tab. Refresh local manifests when the
    // user returns to the console, without polling the public registry.
    refetchOnWindowFocus: 'always',
    retry: false,
  })
}

export function usePlugins() {
  return useQuery(pluginManifestQueryOptions())
}

/** Fetch the signed registry catalog exposed by the backend. */
export function usePluginCatalog(enabled = true) {
  return useQuery({
    queryKey: PLUGIN_CATALOG_QUERY_KEY,
    queryFn: async (): Promise<PluginCatalogResponse> => {
      const response = await listPluginCatalog({ throwOnError: true })
      return response.data
    },
    staleTime: 5 * 60 * 1000,
    retry: false,
    enabled,
  })
}

/** Stop and uninstall the selected plugin, retaining its application data. */
export function useUninstallPlugin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (name: string) => {
      const response = await uninstallPlugin({
        path: { name },
        throwOnError: true,
      })
      return response.data
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}

/** Reload all verified plugin installations from disk. */
export function useReloadPlugins() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (): Promise<ReloadResponse> => {
      const response = await reloadPlugins({ throwOnError: true })
      return response.data
    },
    // A 502 reload can stop every plugin; refresh navigation even on failure.
    onSettled: async () => {
      await queryClient.invalidateQueries({
        queryKey: PLUGINS_QUERY_KEY,
        exact: true,
      })
    },
  })
}
