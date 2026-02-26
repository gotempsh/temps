import { client } from '@/api/client/client.gen'
import type { PluginManifest } from '@/types/plugins'
import { useQuery } from '@tanstack/react-query'

/**
 * Fetch the list of external plugin manifests from /api/x/plugins.
 * This endpoint returns an array of PluginManifest objects for all
 * running external plugins.
 */
async function fetchPluginManifests(): Promise<PluginManifest[]> {
  const response = await client.get<PluginManifest[]>({
    url: '/x/plugins',
  })
  return response.data ?? []
}

/**
 * React Query hook to get the list of external plugins.
 * Caches for 5 minutes since plugins rarely change at runtime.
 */
export function usePlugins() {
  return useQuery({
    queryKey: ['external-plugins'],
    queryFn: fetchPluginManifests,
    staleTime: 5 * 60 * 1000,
    gcTime: 10 * 60 * 1000,
    retry: 1,
  })
}
