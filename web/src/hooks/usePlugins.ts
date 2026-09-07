// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  installPlugin,
  listExternalPlugins,
  listPluginCatalog,
  reloadPlugins,
} from '@/api/client/sdk.gen'
import type {
  InstallPluginResponse,
  PluginCatalogResponse,
  ReloadResponse,
} from '@/api/client/types.gen'
import type { PluginManifest } from '@/types/plugins'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

export const PLUGINS_QUERY_KEY = ['external-plugins']
export const PLUGIN_CATALOG_QUERY_KEY = ['external-plugins', 'catalog']

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
export function usePlugins() {
  return useQuery({
    queryKey: PLUGINS_QUERY_KEY,
    queryFn: fetchPluginManifests,
    staleTime: 5 * 60 * 1000,
    gcTime: 10 * 60 * 1000,
    retry: false,
  })
}

/** Fetch the signed registry catalog exposed by the backend. */
export function usePluginCatalog() {
  return useQuery({
    queryKey: PLUGIN_CATALOG_QUERY_KEY,
    queryFn: async (): Promise<PluginCatalogResponse> => {
      const response = await listPluginCatalog({ throwOnError: true })
      return response.data
    },
    staleTime: 5 * 60 * 1000,
    retry: false,
  })
}

/** Install a named plugin release selected and verified by the backend. */
export function useInstallPlugin() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (name: string): Promise<InstallPluginResponse> => {
      const response = await installPlugin({
        body: { name },
        throwOnError: true,
      })
      return response.data
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY }),
        queryClient.invalidateQueries({ queryKey: PLUGIN_CATALOG_QUERY_KEY }),
      ])
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
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}
