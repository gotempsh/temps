// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { client } from '@/api/client/client.gen'
import type { PluginManifest } from '@/types/plugins'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

export const PLUGINS_QUERY_KEY = ['external-plugins']

/**
 * Fetch the list of external plugin manifests from /api/x/plugins.
 * Returns an empty array if the endpoint is unavailable (e.g., no plugins loaded).
 */
async function fetchPluginManifests(): Promise<PluginManifest[]> {
  try {
    const response = await client.get<PluginManifest[]>({
      url: '/x/plugins',
    })
    return response.data ?? []
  } catch {
    // Endpoint may not exist if no external plugins are configured.
    // Degrade gracefully — no plugins is the default.
    return []
  }
}

/** Response from POST /x/plugins/reload */
export interface ReloadPluginsResponse {
  loaded: number
  plugins: string[]
  message: string
}

/**
 * React Query hook to get the list of external plugins.
 * Caches for 5 minutes since plugins rarely change at runtime.
 * Never throws — returns an empty list on failure.
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

/**
 * Mutation hook to reload all external plugins.
 * On success, invalidates the plugins query so the UI refreshes.
 */
export function useReloadPlugins() {
  const queryClient = useQueryClient()

  return useMutation({
    mutationFn: async (): Promise<ReloadPluginsResponse> => {
      const response = await client.post<ReloadPluginsResponse>({
        url: '/x/plugins/reload',
      })
      return response.data!
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: PLUGINS_QUERY_KEY })
    },
  })
}
